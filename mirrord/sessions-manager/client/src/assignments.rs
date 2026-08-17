use mirrord_sessions_manager_protocol::ConnectionAssignment;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    control_plane::{
        AssignmentSubscription, ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient,
    },
    error::SessionsManagerClientError,
    subscriber::{ControlPlaneSubscriber, ControlPlaneSubscription},
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

    fn extract(&self, event: ControlPlaneEvent) -> Option<Self::Output> {
        match event {
            ControlPlaneEvent::Assignment(assignment) => Some(assignment),
        }
    }
}

pub(crate) struct AgentAssignmentSubscriber(ControlPlaneSubscriber<AssignmentSubscription>);

impl AgentAssignmentSubscriber {
    pub(crate) fn new(
        client: HttpControlPlaneClient,
        replica_id: String,
        cancellation: CancellationToken,
    ) -> Self {
        Self(ControlPlaneSubscriber::new(
            client,
            AssignmentSubscription::Agent { replica_id },
            cancellation,
            true,
        ))
    }

    pub(crate) async fn next(
        &mut self,
    ) -> Option<Result<ConnectionAssignment, SessionsManagerClientError>> {
        self.0.next().await
    }
}

pub(crate) struct IntproxyAssignmentSubscriber(ControlPlaneSubscriber<AssignmentSubscription>);

impl IntproxyAssignmentSubscriber {
    pub(crate) fn new(
        client: HttpControlPlaneClient,
        session_id: String,
        target_replica_id: Option<String>,
        cancellation: CancellationToken,
    ) -> Self {
        Self(ControlPlaneSubscriber::new(
            client,
            AssignmentSubscription::Intproxy {
                session_id,
                target_replica_id,
            },
            cancellation,
            false,
        ))
    }

    pub(crate) async fn next(
        &mut self,
        deadline: Instant,
    ) -> Result<ConnectionAssignment, SessionsManagerClientError> {
        self.0.next_until(deadline).await
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn extracts_assignment_event_unchanged() {
        let subscription = AssignmentSubscription::Agent {
            replica_id: "pod-a".to_owned(),
        };
        let assignment = serde_json::from_value(serde_json::json!({
            "data_plane_endpoint": "/ws/test",
            "authorization": "Bearer test",
        }))
        .unwrap();

        let extracted = subscription
            .extract(ControlPlaneEvent::Assignment(assignment))
            .unwrap();

        assert_eq!(extracted.data_plane_endpoint.as_str(), "/ws/test");
        assert_eq!(extracted.authorization.expose_secret(), "Bearer test");
    }
}
