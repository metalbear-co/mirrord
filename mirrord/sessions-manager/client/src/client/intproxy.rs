use std::{future::Future, sync::Arc, time::Duration};

use mirrord_protocol_io::{Client, Connection, websocket::WebSocketChannel};
use mirrord_sessions_manager_protocol::{
    AssignmentSubscription, ConnectionAssignment, IntproxyConnectionId,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, time::Instant};
use tokio_tungstenite::MaybeTlsStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    client::ClientBuilder,
    config::SessionsManagerConfig,
    control_plane::{HttpControlPlaneClient, subscriber::ControlPlaneSubscriber},
    credentials::{CredentialProvider, credentials_from_env},
    data_plane::{
        DataPlaneConnectRequest, DataPlaneTransport, WebSocketDataPlaneTransport,
        connect_data_plane_raw,
    },
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
        self.retry(deadline, || self.connect_once(deadline)).await
    }

    /// Same as [`Self::connect`], but returns the raw [`WebSocketChannel`] instead of wrapping it
    /// in a [`Connection`].
    ///
    /// [`WebSocketChannel`] already implements [`futures::Sink`] and [`futures::Stream`] directly
    /// over [`mirrord_protocol`](https://docs.rs/mirrord-protocol) messages, so callers that want
    /// to drive the connection themselves (e.g. `mirrord-protocol-api`'s `MirrordClient`) can use
    /// it without going through the [`Connection`] channel abstraction.
    ///
    /// Bypasses [`Self::transport`](Self) and always dials the data plane directly over WebSocket,
    /// since a raw connection is inherently transport-specific.
    pub async fn connect_raw(
        &self,
        timeout: Duration,
    ) -> Result<WebSocketChannel<MaybeTlsStream<TcpStream>, Client>, SessionsManagerClientError>
    {
        let deadline = Instant::now() + timeout;
        self.retry(deadline, || self.connect_once_raw(deadline))
            .await
    }

    /// Retries `attempt` with backoff until it succeeds, `deadline` expires, or the client is
    /// cancelled.
    async fn retry<F, Fut, C>(
        &self,
        deadline: Instant,
        mut attempt: F,
    ) -> Result<C, SessionsManagerClientError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<C, SessionsManagerClientError>>,
    {
        let mut retry_delays = init_retry_policy();

        loop {
            match attempt().await {
                Ok(value) => return Ok(value),
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
        let assignment = self.next_assignment(deadline).await?;

        run_interruptible(
            &self.builder.cancellation,
            Some(self.connect_deadline(deadline)),
            self.builder.transport.connect(DataPlaneConnectRequest {
                control_plane_url: self.builder.config.base_url.clone(),
                assignment,
                credentials: self.builder.credentials.clone(),
            }),
        )
        .await?
    }

    async fn connect_once_raw(
        &self,
        deadline: Instant,
    ) -> Result<WebSocketChannel<MaybeTlsStream<TcpStream>, Client>, SessionsManagerClientError>
    {
        let assignment = self.next_assignment(deadline).await?;

        run_interruptible(
            &self.builder.cancellation,
            Some(self.connect_deadline(deadline)),
            connect_data_plane_raw::<Client>(DataPlaneConnectRequest {
                control_plane_url: self.builder.config.base_url.clone(),
                assignment,
                credentials: self.builder.credentials.clone(),
            }),
        )
        .await?
    }

    async fn next_assignment(
        &self,
        deadline: Instant,
    ) -> Result<ConnectionAssignment, SessionsManagerClientError> {
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
        assignments.next_until(deadline).await
    }

    /// Note: if `deadline` has already passed, the remaining time is zero and the returned
    /// deadline is the current time. The subsequent `run_interruptible` call will immediately
    /// time out, which is the desired behavior. No explicit deadline check is needed.
    fn connect_deadline(&self, deadline: Instant) -> Instant {
        let remaining = deadline.saturating_duration_since(Instant::now());
        Instant::now() + remaining.min(self.builder.transport.connect_timeout())
    }
}
