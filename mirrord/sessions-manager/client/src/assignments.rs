use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use mirrord_sessions_manager_protocol::{
    AgentIdentity, AssignmentId, AssignmentSubscription, ConnectionAssignment,
};

use crate::{
    control_plane::{
        ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient,
        subscriber::{ControlPlaneSubscriber, ControlPlaneSubscription},
    },
    error::SessionsManagerClientError,
    retry::RetryBudget,
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
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        client.subscribe_assignments(self).await
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
/// Bounds IDs left `Connecting` when a panicked upgrade task cannot report which assignment
/// failed. Without it, that ID would suppress every replay indefinitely.
const CONNECTING_ASSIGNMENT_TTL: Duration = Duration::from_secs(2 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignmentState {
    Connecting { accepted_at: Instant },
    Connected { completed_at: Instant },
}

/// Prevents an agent from acting on the same assignment more than once across control-plane
/// reconnects.
///
/// Sessions-manager can replay an assignment when an SSE subscription is reopened before it has
/// observed the agent's acknowledgement. Starting a second data-plane upgrade for that replay can
/// create two local connections for one assignment, so IDs stay registered while an upgrade is in
/// progress and after it succeeds. A failed upgrade removes its ID so sessions-manager can retry
/// it. Completed IDs expire by age and are also kept in completion order, bounding memory when the
/// server produces many assignments.
#[derive(Debug, Default)]
pub(crate) struct AssignmentRegistry {
    states: HashMap<AssignmentId, AssignmentState>,
    completed_order: VecDeque<AssignmentId>,
}

impl AssignmentRegistry {
    /// Removes every entry whose TTL has elapsed. A full scan over `states`, so callers only run
    /// it once per operation rather than repeatedly within the same call.
    fn prune_expired(&mut self, now: Instant) {
        let expired = self
            .states
            .iter()
            .filter(|(_, state)| match state {
                AssignmentState::Connected { completed_at } => {
                    now.duration_since(*completed_at) >= COMPLETED_ASSIGNMENT_TTL
                }
                AssignmentState::Connecting { accepted_at } => {
                    now.duration_since(*accepted_at) >= CONNECTING_ASSIGNMENT_TTL
                }
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            self.states.remove(&id);
        }
        self.completed_order
            .retain(|id| self.states.contains_key(id));
    }

    /// Evicts the oldest completed entries once the completion queue exceeds its capacity. Unlike
    /// [`Self::prune_expired`] this only touches the front of the queue, so it's cheap enough to
    /// call after every insertion.
    fn trim_completed_assignments(&mut self) {
        while self.completed_order.len() > COMPLETED_ASSIGNMENT_CAPACITY {
            let Some(id) = self.completed_order.pop_front() else {
                break;
            };
            if matches!(
                self.states.get(&id),
                Some(AssignmentState::Connected { .. })
            ) {
                self.states.remove(&id);
            }
        }
    }

    fn try_accept(&mut self, id: AssignmentId) -> bool {
        self.prune_expired(Instant::now());
        if self.states.contains_key(&id) {
            return false;
        }
        self.states.insert(
            id,
            AssignmentState::Connecting {
                accepted_at: Instant::now(),
            },
        );
        true
    }

    pub(crate) fn mark_connected(&mut self, id: &AssignmentId) {
        self.prune_expired(Instant::now());
        if self.states.contains_key(id) {
            self.states.insert(
                id.clone(),
                AssignmentState::Connected {
                    completed_at: Instant::now(),
                },
            );
            self.completed_order.push_back(id.clone());
            self.trim_completed_assignments();
        }
    }

    pub(crate) fn release_for_retry(&mut self, id: &AssignmentId) {
        self.states.remove(id);
        self.completed_order.retain(|candidate| candidate != id);
    }
}

/// Filters replayed assignments from the agent control-plane subscription.
pub(crate) struct DeduplicatingAssignmentSubscriber {
    subscriber: ControlPlaneSubscriber<AssignmentSubscription>,
    assignments: AssignmentRegistry,
    retry: RetryBudget,
}

impl DeduplicatingAssignmentSubscriber {
    pub(crate) fn new(client: HttpControlPlaneClient, identity: AgentIdentity) -> Self {
        Self {
            subscriber: ControlPlaneSubscriber::new(
                client,
                AssignmentSubscription::Agent(identity),
                true,
            ),
            assignments: AssignmentRegistry::default(),
            retry: RetryBudget::new(),
        }
    }

    pub(crate) async fn next(
        &mut self,
    ) -> Result<ConnectionAssignment, SessionsManagerClientError> {
        loop {
            let assignment = self.subscriber.next(None).await?;
            if self
                .assignments
                .try_accept(assignment.assignment_id.clone())
            {
                return Ok(assignment);
            }
        }
    }

    pub(crate) async fn retry_assignment(&mut self, assignment_id: &AssignmentId) {
        let retry_delay = self.retry.wait_next_delay().await;
        tracing::warn!(%assignment_id, ?retry_delay, "failed to connect sessions-manager data-plane, retrying assignment");
        self.assignments.release_for_retry(assignment_id);
        // Keep SSE connection alive; server will send next assignment or Superseded if session is
        // invalid. Assignment deduplication handles replays from server.
    }

    pub(crate) fn ack_connected(&mut self, assignment_id: &AssignmentId) {
        self.assignments.mark_connected(assignment_id);
        self.retry.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connecting_entries_survive_pruning_within_their_ttl() {
        let mut registry = AssignmentRegistry::default();
        let id = AssignmentId::from("assignment".to_owned());
        assert!(registry.try_accept(id.clone()));
        registry.prune_expired(Instant::now() + CONNECTING_ASSIGNMENT_TTL / 2);
        assert!(!registry.try_accept(id));
    }

    #[test]
    fn connecting_entries_are_evicted_after_their_ttl_expires() {
        // Simulates an assignment whose data-plane upgrade task panicked: nothing ever calls
        // `mark_connected()` or `release_for_retry()` for it, so only TTL-based pruning can
        // reclaim the slot.
        let mut registry = AssignmentRegistry::default();
        let id = AssignmentId::from("assignment".to_owned());
        assert!(registry.try_accept(id.clone()));
        registry.prune_expired(Instant::now() + CONNECTING_ASSIGNMENT_TTL * 2);
        assert!(registry.try_accept(id));
    }

    #[test]
    fn release_for_retry_removes_connecting_entry() {
        let mut registry = AssignmentRegistry::default();
        let id = AssignmentId::from("assignment".to_owned());
        assert!(registry.try_accept(id.clone()));
        registry.release_for_retry(&id);
        assert!(registry.try_accept(id));
    }
}
