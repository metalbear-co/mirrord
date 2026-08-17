use std::{sync::Arc, time::Duration};

use mirrord_protocol_io::{Client, Connection};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, time::Instant};
use tokio_tungstenite::MaybeTlsStream;
use tokio_util::sync::CancellationToken;

use crate::{
    assignments::IntproxyAssignmentSubscriber,
    config::SessionsManagerConfig,
    control_plane::HttpControlPlaneClient,
    credentials::{CredentialProvider, NoCredentials},
    data_plane::{BinaryWebSocketConnection, connect_data_plane_raw},
    error::SessionsManagerClientError,
    retry::run_interruptible,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsManagerConnectInfo {
    pub service: String,
    pub environment: String,
    pub target_replica_id: Option<String>,
    pub session_id: String,
}

pub struct IntproxyClient {
    config: SessionsManagerConfig,
    session_id: String,
    target_replica_id: Option<String>,
    cancellation: CancellationToken,
    credentials: Arc<dyn CredentialProvider>,
}

impl IntproxyClient {
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
        })
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    pub async fn connect(
        &self,
        timeout: Duration,
    ) -> Result<Connection<Client>, SessionsManagerClientError> {
        Ok(Connection::from_channel(self.connect_raw(timeout).await?))
    }

    /// Same as [`Self::connect`], but returns the raw [`BinaryWebSocketConnection`] instead of
    /// wrapping it in a [`Connection`].
    ///
    /// [`BinaryWebSocketConnection`] already implements [`futures::Sink`] and
    /// [`futures::Stream`] directly over [`mirrord_protocol`](https://docs.rs/mirrord-protocol)
    /// messages, so callers that want to drive the connection themselves (e.g.
    /// `mirrord-protocol-api`'s `MirrordClient`) can use it without going through the
    /// [`Connection`] channel abstraction.
    pub async fn connect_raw(
        &self,
        timeout: Duration,
    ) -> Result<
        BinaryWebSocketConnection<MaybeTlsStream<TcpStream>, Client>,
        SessionsManagerClientError,
    > {
        let deadline = Instant::now() + timeout;
        let client = HttpControlPlaneClient::new(&self.config, self.credentials.clone())?;
        let mut assignments = IntproxyAssignmentSubscriber::new(
            client,
            self.session_id.clone(),
            self.target_replica_id.clone(),
            self.cancellation.clone(),
        );
        let assignment = assignments.next(deadline).await?;

        run_interruptible(
            &self.cancellation,
            Some(deadline),
            connect_data_plane_raw::<Client>(&self.config.base_url, assignment),
        )
        .await?
    }
}
