use std::{future::Future, sync::Arc, time::Duration};

use mirrord_protocol_io::{Client, Connection, websocket::WebSocketChannel};
use mirrord_sessions_manager_protocol::ConnectionAssignment;
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, time::Instant};
use tokio_tungstenite::MaybeTlsStream;
use tokio_util::sync::CancellationToken;

use crate::{
    assignments::IntproxyAssignmentSubscriber,
    config::SessionsManagerConfig,
    control_plane::HttpControlPlaneClient,
    credentials::{CredentialProvider, NoCredentials},
    data_plane::{
        DataPlaneConnectRequest, DataPlaneTransport, WebSocketDataPlaneTransport,
        connect_data_plane_raw,
    },
    error::{RetryDisposition, SessionsManagerClientError},
    retry::{init_retry_policy, run_interruptible, wait_retry},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsManagerConnectInfo {
    pub service: String,
    pub environment: String,
    pub target_replica_id: Option<String>,
    pub session_id: String,
}

pub struct IntproxyClient<T = WebSocketDataPlaneTransport> {
    config: SessionsManagerConfig,
    session_id: String,
    target_replica_id: Option<String>,
    cancellation: CancellationToken,
    credentials: Arc<dyn CredentialProvider>,
    transport: T,
}

impl IntproxyClient<WebSocketDataPlaneTransport> {
    pub fn new(
        connect_info: SessionsManagerConnectInfo,
        cancellation: impl Into<Option<CancellationToken>>,
    ) -> Result<Self, SessionsManagerClientError> {
        Ok(Self {
            config: SessionsManagerConfig::new(connect_info.environment, connect_info.service)?,
            session_id: connect_info.session_id,
            target_replica_id: connect_info.target_replica_id,
            cancellation: cancellation.into().unwrap_or_default(),
            credentials: Arc::new(NoCredentials),
            transport: WebSocketDataPlaneTransport,
        })
    }
}

impl<T: DataPlaneTransport> IntproxyClient<T> {
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    pub fn with_data_plane_transport<U: DataPlaneTransport>(
        self,
        transport: U,
    ) -> IntproxyClient<U> {
        IntproxyClient {
            config: self.config,
            session_id: self.session_id,
            target_replica_id: self.target_replica_id,
            cancellation: self.cancellation,
            credentials: self.credentials,
            transport,
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
                Err(error) if error.retry_disposition() == RetryDisposition::Fatal => {
                    return Err(error);
                }
                Err(error) => {
                    let retry_delay = retry_delays
                        .next()
                        .expect("exponential backoff strategy is unbounded");
                    tracing::warn!(%error, ?retry_delay, "sessions-manager intproxy setup failed");
                    wait_retry(&self.cancellation, Some(deadline), retry_delay).await?;
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
            &self.cancellation,
            Some(self.connect_deadline(deadline)),
            self.transport.connect(DataPlaneConnectRequest {
                control_plane_url: self.config.base_url.clone(),
                assignment,
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
            &self.cancellation,
            Some(self.connect_deadline(deadline)),
            connect_data_plane_raw::<Client>(DataPlaneConnectRequest {
                control_plane_url: self.config.base_url.clone(),
                assignment,
            }),
        )
        .await?
    }

    async fn next_assignment(
        &self,
        deadline: Instant,
    ) -> Result<ConnectionAssignment, SessionsManagerClientError> {
        let client = HttpControlPlaneClient::new(&self.config, self.credentials.clone())?;
        let mut assignments = IntproxyAssignmentSubscriber::new(
            client,
            self.session_id.clone(),
            self.target_replica_id.clone(),
            self.cancellation.clone(),
        );
        assignments.next(deadline).await
    }

    /// Note: if `deadline` has already passed, the remaining time is zero and the returned
    /// deadline is the current time. The subsequent `run_interruptible` call will immediately
    /// time out, which is the desired behavior. No explicit deadline check is needed.
    fn connect_deadline(&self, deadline: Instant) -> Instant {
        let remaining = deadline.saturating_duration_since(Instant::now());
        Instant::now() + remaining.min(self.transport.connect_timeout())
    }
}
