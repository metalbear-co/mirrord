use std::{future::Future, time::Duration};

use mirrord_operator_websocket::connection::OperatorConnection;
use mirrord_protocol_io::Client;
use mirrord_sessions_manager_protocol::{
    AssignmentSubscription, ConnectionAssignment, IntproxyIdentity, ReplicaId, ServiceScope,
};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::{
    client::validate_scope,
    control_plane::subscriber::ControlPlaneSubscriber,
    error::SessionsManagerClientError,
    retry::{RetryBudget, with_deadline},
    transport::{DirectTransport, SessionsManagerTransport},
};

/// Describes the sessions-manager control-plane subscription an intproxy opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsManagerConnectInfo {
    /// Service scope for this control-plane registration.
    pub scope: ServiceScope,
    /// Identifies the local mirrord session this connection belongs to.
    ///
    /// Each [`IntproxyClient`] created for this session receives its own independently allocated
    /// connection identity.
    pub user_session_id: String,
    /// Optional replica constraint for routing this connection.
    pub replica_filter: Option<ReplicaId>,
}

/// Connects an intproxy to the data plane assigned by sessions-manager.
pub struct IntproxyClient<T = DirectTransport> {
    scope: ServiceScope,
    /// The connection id isolates this client's allocation from other intproxies in the user
    /// session while staying stable across the control-plane subscriber's SSE reconnects.
    identity: IntproxyIdentity,
    replica_filter: Option<ReplicaId>,
    transport: T,
}

impl IntproxyClient<DirectTransport> {
    pub fn new(
        connect_info: SessionsManagerConnectInfo,
    ) -> Result<Self, SessionsManagerClientError> {
        Self::with_transport(connect_info, DirectTransport::from_env()?)
    }
}

impl<T: SessionsManagerTransport> IntproxyClient<T> {
    pub fn with_transport(
        connect_info: SessionsManagerConnectInfo,
        transport: T,
    ) -> Result<Self, SessionsManagerClientError> {
        Ok(Self {
            scope: validate_scope(connect_info.scope)?,
            identity: IntproxyIdentity {
                user_session_id: connect_info.user_session_id,
                intproxy_connection_id: uuid::Uuid::new_v4().to_string(),
            },
            replica_filter: connect_info.replica_filter,
            transport,
        })
    }

    /// Waits for an assignment and connects to the data plane it names, retrying within `timeout`.
    ///
    /// The returned connection exposes incoming daemon messages and accepts typed client messages
    /// or pre-encoded binary payloads, allowing callers to drive its [`futures::Sink`] and
    /// [`futures::Stream`] implementations directly.
    pub async fn connect(
        &self,
        timeout: Duration,
    ) -> Result<OperatorConnection<Client>, SessionsManagerClientError> {
        let deadline = Instant::now() + timeout;
        self.retry(deadline, || self.connect_once(deadline)).await
    }

    /// Retries `attempt` with backoff until it succeeds or `deadline` expires.
    async fn retry<F, Fut, R>(
        &self,
        deadline: Instant,
        attempt: F,
    ) -> Result<R, SessionsManagerClientError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, SessionsManagerClientError>>,
    {
        RetryBudget::new()
            .run_until(
                Some(deadline),
                attempt,
                |error: &SessionsManagerClientError| {
                    let retryable = error.is_retryable();
                    if retryable {
                        tracing::warn!(%error, "sessions-manager intproxy setup failed, retrying");
                    }
                    retryable
                },
            )
            .await
    }

    async fn connect_once(
        &self,
        deadline: Instant,
    ) -> Result<OperatorConnection<Client>, SessionsManagerClientError> {
        let assignment = self.next_assignment(deadline).await?;

        with_deadline(
            Some(self.connect_deadline(deadline)),
            self.transport.connect_data_plane(assignment),
        )
        .await?
    }

    async fn next_assignment(
        &self,
        deadline: Instant,
    ) -> Result<ConnectionAssignment, SessionsManagerClientError> {
        let mut assignments = ControlPlaneSubscriber::new(
            self.transport.clone(),
            self.scope.clone(),
            AssignmentSubscription::Intproxy {
                identity: self.identity.clone(),
                agent_replica_filter: self.replica_filter.clone(),
            },
            false,
        );
        assignments.next(Some(deadline)).await
    }

    /// Note: if `deadline` has already passed, the remaining time is zero and the returned
    /// deadline is the current time. The subsequent `with_deadline` call will immediately time
    /// out, which is the desired behavior. No explicit deadline check is needed.
    fn connect_deadline(&self, deadline: Instant) -> Instant {
        let remaining = deadline.saturating_duration_since(Instant::now());
        Instant::now() + remaining.min(self.transport.connect_timeout())
    }
}

#[cfg(test)]
mod tests {
    use mirrord_sessions_manager_protocol::IntproxyIdentity;

    fn new_identity(user_session_id: String) -> IntproxyIdentity {
        IntproxyIdentity {
            user_session_id,
            intproxy_connection_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    #[test]
    fn connection_identity_preserves_its_user_session_and_is_unique() {
        let first = new_identity("session".to_owned());
        let second = new_identity("session".to_owned());

        assert_eq!(first.user_session_id, "session");
        assert_eq!(second.user_session_id, "session");
        assert_ne!(first.intproxy_connection_id, second.intproxy_connection_id);
    }
}
