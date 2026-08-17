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
    data_plane::connect_data_plane,
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
            connect_data_plane::<Client>(&self.config.base_url, assignment),
        )
        .await?
    }
}
