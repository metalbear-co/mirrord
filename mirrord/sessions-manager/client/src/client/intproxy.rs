use std::{sync::Arc, time::Duration};

use mirrord_protocol_io::{Client, Connection};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    assignments::IntproxyAssignmentSubscriber,
    config::SessionsManagerConfig,
    control_plane::HttpControlPlaneClient,
    credentials::{CredentialProvider, NoCredentials},
    data_plane::{DataPlaneTransport, WebSocketDataPlaneTransport},
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
        let mut retry_delays = init_retry_policy();

        loop {
            match self.connect_once(deadline).await {
                Ok(connection) => return Ok(connection),
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
        let client = HttpControlPlaneClient::new(&self.config, self.credentials.clone())?;
        let mut assignments = IntproxyAssignmentSubscriber::new(
            client,
            self.session_id.clone(),
            self.target_replica_id.clone(),
            self.cancellation.clone(),
        );
        let assignment = assignments.next(deadline).await?;

        let remaining = deadline.saturating_duration_since(Instant::now());
        // Note: if deadline has already passed, remaining is zero and connect_deadline becomes
        // current time. The subsequent run_interruptible call will immediately timeout, which is
        // the desired behavior. No explicit deadline check is needed.
        let connect_deadline = Instant::now() + remaining.min(self.transport.connect_timeout());
        run_interruptible(
            &self.cancellation,
            Some(connect_deadline),
            self.transport
                .connect(crate::data_plane::DataPlaneConnectRequest {
                    control_plane_url: self.config.base_url.clone(),
                    assignment,
                }),
        )
        .await?
    }
}
