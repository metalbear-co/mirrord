use std::{sync::Arc, time::Duration};

use mirrord_protocol_io::{Client, Connection};
use mirrord_sessions_manager_protocol::{AssignmentSubscription, IntproxyConnectionId};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    client::ClientBuilder,
    config::SessionsManagerConfig,
    control_plane::{HttpControlPlaneClient, subscriber::ControlPlaneSubscriber},
    credentials::{CredentialProvider, credentials_from_env},
    data_plane::{DataPlaneTransport, WebSocketDataPlaneTransport},
    error::SessionsManagerClientError,
    retry::{init_retry_policy, run_interruptible, wait_next_retry_delay},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsManagerConnectInfo {
    pub service: String,
    pub environment: String,
    pub agent_replica_filter: Option<String>,
    pub user_session_id: String,
}

pub struct IntproxyClient<T = WebSocketDataPlaneTransport> {
    user_session_id: String,
    /// Isolates this client's allocation from other intproxies in the user session while staying
    /// stable across the control-plane subscriber's SSE reconnects.
    intproxy_connection_id: IntproxyConnectionId,
    agent_replica_filter: Option<String>,
    builder: ClientBuilder<T>,
}

impl IntproxyClient<WebSocketDataPlaneTransport> {
    pub fn new(
        connect_info: SessionsManagerConnectInfo,
        cancellation: impl Into<Option<CancellationToken>>,
    ) -> Result<Self, SessionsManagerClientError> {
        Ok(Self {
            user_session_id: connect_info.user_session_id,
            intproxy_connection_id: Uuid::new_v4().to_string().into(),
            agent_replica_filter: connect_info.agent_replica_filter,
            builder: ClientBuilder {
                config: SessionsManagerConfig::new(
                    connect_info.environment,
                    connect_info.service,
                    SessionsManagerConfig::base_url_from_env()?,
                )?,
                credentials: credentials_from_env()?,
                cancellation: cancellation.into().unwrap_or_default(),
                transport: WebSocketDataPlaneTransport,
            },
        })
    }
}

impl<T: DataPlaneTransport> IntproxyClient<T> {
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.builder = self.builder.with_credentials(credentials);
        self
    }

    pub fn with_transport<U: DataPlaneTransport>(self, transport: U) -> IntproxyClient<U> {
        IntproxyClient {
            user_session_id: self.user_session_id,
            intproxy_connection_id: self.intproxy_connection_id,
            agent_replica_filter: self.agent_replica_filter,
            builder: self.builder.with_transport(transport),
        }
    }

    pub async fn connect(
        &self,
        timeout: Duration,
    ) -> Result<Connection<Client>, SessionsManagerClientError> {
        let deadline = Instant::now() + timeout;
        let mut retry_delays = init_retry_policy();

        loop {
            match self.connect_once(deadline).await {
                Ok(connection) => return Ok(connection),
                Err(SessionsManagerClientError::Cancelled) => {
                    return Err(SessionsManagerClientError::Cancelled);
                }
                Err(error) if !error.is_retryable() => {
                    return Err(error);
                }
                Err(error) => {
                    let retry_delay = wait_next_retry_delay(
                        &mut retry_delays,
                        &self.builder.cancellation,
                        Some(deadline),
                    )
                    .await?;
                    tracing::warn!(%error, ?retry_delay, "sessions-manager intproxy setup failed");
                }
            }
        }
    }

    async fn connect_once(
        &self,
        deadline: Instant,
    ) -> Result<Connection<Client>, SessionsManagerClientError> {
        let client =
            HttpControlPlaneClient::new(&self.builder.config, self.builder.credentials.clone())?;
        let mut assignments = ControlPlaneSubscriber::new(
            client,
            AssignmentSubscription::Intproxy {
                user_session_id: self.user_session_id.clone(),
                intproxy_connection_id: self.intproxy_connection_id.clone(),
                agent_replica_filter: self.agent_replica_filter.clone(),
            },
            self.builder.cancellation.clone(),
            false,
        );
        // May reconnect the control-plane subscription internally, even while `deadline` is
        // still far off, if the connection goes silent for a while — see
        // `ControlPlaneSubscriber::next_until`.
        let assignment = assignments.next_until(deadline).await?;

        let remaining = deadline.saturating_duration_since(Instant::now());
        // Note: if deadline has already passed, remaining is zero and connect_deadline becomes
        // current time. The subsequent run_interruptible call will immediately timeout, which is
        // the desired behavior. No explicit deadline check is needed.
        let connect_deadline =
            Instant::now() + remaining.min(self.builder.transport.connect_timeout());
        run_interruptible(
            &self.builder.cancellation,
            Some(connect_deadline),
            self.builder
                .transport
                .connect(crate::data_plane::DataPlaneConnectRequest {
                    control_plane_url: self.builder.config.base_url.clone(),
                    assignment,
                    credentials: self.builder.credentials.clone(),
                }),
        )
        .await?
    }
}
