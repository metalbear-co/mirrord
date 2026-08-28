use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use mirrord_sessions_manager_protocol::{
    AssignmentId, AssignmentSubscription, ConnectionAssignment,
};
use tokio_util::sync::CancellationToken;

use crate::{
    control_plane::{
        ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient,
        subscriber::{ControlPlaneSubscriber, ControlPlaneSubscription},
    },
    error::SessionsManagerClientError,
    retry::{RetryDelays, init_retry_policy, wait_next_retry_delay},
};

impl ControlPlaneSubscription for AssignmentSubscription {
    type Output = ConnectionAssignment;

    fn name(&self) -> &'static str {
        match self {
            Self::Agent { .. } => "agent assignments",
            Self::Intproxy { .. } => "intproxy assignments",
        }
    }

    async fn subscribe(
        &self,
        client: &HttpControlPlaneClient,
        cancellation: &CancellationToken,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        client.subscribe_assignments(self, cancellation).await
    }

    fn extract(
        &self,
        event: ControlPlaneEvent,
    ) -> Result<Self::Output, SessionsManagerClientError> {
        match event {
            ControlPlaneEvent::Assignment(assignment) => Ok(assignment),
            ControlPlaneEvent::Superseded => Err(SessionsManagerClientError::Superseded),
        }
    }
}

const COMPLETED_ASSIGNMENT_CAPACITY: usize = 1_024;
const COMPLETED_ASSIGNMENT_TTL: Duration = Duration::from_secs(10 * 60);
/// How long an assignment may stay `Connecting` before it's treated as abandoned and evicted.
///
/// Normally an assignment leaves `Connecting` via `connected()` or `retry()`. But if its
/// data-plane upgrade task panics instead of returning an error, the surrounding `JoinError`
/// carries no assignment id, so neither of those is called and the entry would otherwise be stuck
/// in `Connecting` forever — permanently blocking `accept()` from ever reclaiming that id. This
/// TTL bounds that failure mode instead of requiring it to never happen.
const CONNECTING_ASSIGNMENT_TTL: Duration = Duration::from_secs(2 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalAssignmentState {
    Connecting { accepted_at: Instant },
    Connected { completed_at: Instant },
}

#[derive(Debug, Default)]
pub(crate) struct AssignmentRegistry {
    states: HashMap<AssignmentId, LocalAssignmentState>,
    completed_lru: VecDeque<AssignmentId>,
}

impl AssignmentRegistry {
    /// Removes every entry whose TTL has elapsed. A full scan over `states`, so callers only run
    /// it once per operation rather than repeatedly within the same call.
    fn prune_expired(&mut self, now: Instant) {
        let expired = self
            .states
            .iter()
            .filter(|(_, state)| match state {
                LocalAssignmentState::Connected { completed_at } => {
                    now.duration_since(*completed_at) >= COMPLETED_ASSIGNMENT_TTL
                }
                LocalAssignmentState::Connecting { accepted_at } => {
                    now.duration_since(*accepted_at) >= CONNECTING_ASSIGNMENT_TTL
                }
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            self.states.remove(&id);
        }
        self.completed_lru.retain(|id| self.states.contains_key(id));
    }

    /// Evicts the oldest completed entries once `completed_lru` exceeds its capacity. Unlike
    /// [`Self::prune_expired`] this only touches the front of the queue, so it's cheap enough to
    /// call after every insertion.
    fn trim_completed_lru(&mut self) {
        while self.completed_lru.len() > COMPLETED_ASSIGNMENT_CAPACITY {
            let Some(id) = self.completed_lru.pop_front() else {
                break;
            };
            if matches!(
                self.states.get(&id),
                Some(LocalAssignmentState::Connected { .. })
            ) {
                self.states.remove(&id);
            }
        }
    }

    fn accept(&mut self, id: AssignmentId) -> bool {
        self.prune_expired(Instant::now());
        if self.states.contains_key(&id) {
            return false;
        }
        self.states.insert(
            id,
            LocalAssignmentState::Connecting {
                accepted_at: Instant::now(),
            },
        );
        true
    }

    pub(crate) fn connected(&mut self, id: &AssignmentId) {
        self.prune_expired(Instant::now());
        if self.states.contains_key(id) {
            self.states.insert(
                id.clone(),
                LocalAssignmentState::Connected {
                    completed_at: Instant::now(),
                },
            );
            self.completed_lru.push_back(id.clone());
            self.trim_completed_lru();
        }
    }

    pub(crate) fn retry(&mut self, id: &AssignmentId) {
        self.states.remove(id);
        self.completed_lru.retain(|candidate| candidate != id);
    }
}

pub(crate) struct AgentAssignmentSubscriber {
    subscriber: ControlPlaneSubscriber<AssignmentSubscription>,
    assignments: AssignmentRegistry,
    cancellation: CancellationToken,
    retry_delays: RetryDelays,
}

impl AgentAssignmentSubscriber {
    pub(crate) fn new(
        client: HttpControlPlaneClient,
        replica_id: String,
        agent_instance_id: String,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            subscriber: ControlPlaneSubscriber::new(
                client,
                AssignmentSubscription::Agent {
                    replica_id,
                    agent_instance_id: agent_instance_id.into(),
                },
                cancellation.clone(),
                true,
            ),
            assignments: AssignmentRegistry::default(),
            cancellation,
            retry_delays: init_retry_policy(),
        }
    }

    pub(crate) async fn next(
        &mut self,
    ) -> Option<Result<ConnectionAssignment, SessionsManagerClientError>> {
        loop {
            let assignment = self.subscriber.next().await?;
            match assignment {
                Ok(assignment) if self.assignments.accept(assignment.assignment_id.clone()) => {
                    return Some(Ok(assignment));
                }
                Ok(_) => continue,
                Err(error) => return Some(Err(error)),
            }
        }
    }

    pub(crate) async fn retry(
        &mut self,
        assignment_id: &AssignmentId,
    ) -> Result<(), SessionsManagerClientError> {
        let retry_delay =
            wait_next_retry_delay(&mut self.retry_delays, &self.cancellation, None).await?;
        tracing::warn!(%assignment_id, ?retry_delay, "failed to connect sessions-manager data-plane, retrying assignment");
        self.assignments.retry(assignment_id);
        // Keep SSE connection alive; server will send next assignment or Superseded if session is
        // invalid. Assignment deduplication handles replays from server.
        Ok(())
    }

    pub(crate) fn ack_connected(&mut self, assignment_id: &AssignmentId) {
        self.assignments.connected(assignment_id);
        self.retry_delays = init_retry_policy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connecting_entries_survive_pruning_within_their_ttl() {
        let mut registry = AssignmentRegistry::default();
        let id = AssignmentId::from("assignment".to_owned());
        assert!(registry.accept(id.clone()));
        registry.prune_expired(Instant::now() + CONNECTING_ASSIGNMENT_TTL / 2);
        assert!(!registry.accept(id));
    }

    #[test]
    fn connecting_entries_are_evicted_after_their_ttl_expires() {
        // Simulates an assignment whose data-plane upgrade task panicked: nothing ever calls
        // `connected()` or `retry()` for it, so only TTL-based pruning can reclaim the slot.
        let mut registry = AssignmentRegistry::default();
        let id = AssignmentId::from("assignment".to_owned());
        assert!(registry.accept(id.clone()));
        registry.prune_expired(Instant::now() + CONNECTING_ASSIGNMENT_TTL * 2);
        assert!(registry.accept(id));
    }

    #[test]
    fn retry_removes_connecting_entry() {
        let mut registry = AssignmentRegistry::default();
        let id = AssignmentId::from("assignment".to_owned());
        assert!(registry.accept(id.clone()));
        registry.retry(&id);
        assert!(registry.accept(id));
    }
}
