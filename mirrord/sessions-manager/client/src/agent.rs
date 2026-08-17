use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use mirrord_protocol_io::{Agent, Connection};
use mirrord_sessions_manager_protocol::ConnectionAssignment;
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    assignments::AgentAssignmentSubscriber,
    config::SessionsManagerConfig,
    control_plane::HttpControlPlaneClient,
    credentials::{CredentialProvider, NoCredentials},
    data_plane::connect_data_plane,
    env::sessions_manager_environment,
    error::SessionsManagerClientError,
    retry::run_interruptible,
};

const UNBOUNDED_QUEUE_WARNING_INTERVAL: usize = 100;

fn should_warn_about_queue_size(size: usize) -> bool {
    size != 0 && size.is_multiple_of(UNBOUNDED_QUEUE_WARNING_INTERVAL)
}

pub struct AgentClient {
    config: SessionsManagerConfig,
    replica_id: String,
    cancellation: CancellationToken,
    credentials: Arc<dyn CredentialProvider>,
}

impl AgentClient {
    pub fn new(
        service: impl Into<String>,
        replica_id: impl Into<String>,
        cancellation: impl Into<Option<CancellationToken>>,
    ) -> Result<Self, SessionsManagerClientError> {
        Ok(Self {
            config: SessionsManagerConfig::new(
                sessions_manager_environment().unwrap_or_else(|| "default".to_owned()),
                service.into(),
            )?,
            replica_id: replica_id.into(),
            cancellation: cancellation.into().unwrap_or_default(),
            credentials: Arc::new(NoCredentials),
        })
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    pub fn start_control_plane(self) -> Result<AgentControlPlane, SessionsManagerClientError> {
        let client = HttpControlPlaneClient::new(&self.config, self.credentials)?;

        Ok(AgentControlPlane::start(
            client,
            self.replica_id,
            self.config.base_url,
            self.cancellation,
        ))
    }
}

pub struct AgentControlPlane {
    connections: mpsc::UnboundedReceiver<Connection<Agent>>,
    queued_connections: Arc<AtomicUsize>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<Result<(), SessionsManagerClientError>>>,
}

impl AgentControlPlane {
    fn start(
        client: HttpControlPlaneClient,
        replica_id: String,
        data_plane_base_url: Url,
        cancellation: CancellationToken,
    ) -> Self {
        let (connections_tx, connections) = mpsc::unbounded_channel();
        let queued_connections = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn(Self::run(
            client,
            replica_id,
            data_plane_base_url,
            cancellation.clone(),
            connections_tx,
            queued_connections.clone(),
        ));

        Self {
            connections,
            queued_connections,
            cancellation,
            task: Some(task),
        }
    }

    async fn run(
        client: HttpControlPlaneClient,
        replica_id: String,
        data_plane_base_url: Url,
        cancellation: CancellationToken,
        connections_tx: mpsc::UnboundedSender<Connection<Agent>>,
        queued_connections: Arc<AtomicUsize>,
    ) -> Result<(), SessionsManagerClientError> {
        let mut dataplane_upgrades = JoinSet::new();
        let mut assignments =
            AgentAssignmentSubscriber::new(client, replica_id, cancellation.clone());
        let result = loop {
            tokio::select! {
                _ = cancellation.cancelled() => break Ok(()),
                assignment = assignments.next() => match assignment {
                    Some(Ok(assignment)) => Self::start_data_plane_upgrade(
                        &mut dataplane_upgrades,
                        &data_plane_base_url,
                        &cancellation,
                        assignment,
                    ),
                    Some(Err(error)) => break Err(error),
                    None => break Ok(()),
                },
                result = dataplane_upgrades.join_next(), if !dataplane_upgrades.is_empty() => {
                    match result {
                        Some(Ok(Ok(connection))) => {
                            let pending_connections =
                                queued_connections.fetch_add(1, Ordering::Relaxed) + 1;
                            if connections_tx.send(connection).is_ok() {
                                if should_warn_about_queue_size(pending_connections) {
                                    tracing::warn!(
                                        pending_connections,
                                        "sessions-manager data-plane connections are accumulating"
                                    );
                                }
                            } else {
                                queued_connections.fetch_sub(1, Ordering::Relaxed);
                            }
                        }
                        Some(Ok(Err(error))) => tracing::warn!(%error, "failed to connect sessions-manager data plane"),
                        Some(Err(error)) if !error.is_cancelled() => tracing::warn!(%error, "sessions-manager data-plane task failed"),
                        _ => {}
                    }
                }
            }
        };
        dataplane_upgrades.abort_all();
        while dataplane_upgrades.join_next().await.is_some() {}
        result
    }

    fn start_data_plane_upgrade(
        dataplane_upgrades: &mut JoinSet<Result<Connection<Agent>, SessionsManagerClientError>>,
        data_plane_base_url: &Url,
        cancellation: &CancellationToken,
        assignment: ConnectionAssignment,
    ) {
        let base_url = data_plane_base_url.clone();
        let cancellation = cancellation.clone();
        dataplane_upgrades.spawn(async move {
            run_interruptible(
                &cancellation,
                None,
                connect_data_plane::<Agent>(&base_url, assignment),
            )
            .await?
        });

        let pending_upgrades = dataplane_upgrades.len();
        if should_warn_about_queue_size(pending_upgrades) {
            tracing::warn!(
                pending_upgrades,
                "sessions-manager data-plane upgrades are accumulating"
            );
        }
    }

    pub async fn recv(&mut self) -> Option<Connection<Agent>> {
        let connection = self.connections.recv().await;
        if connection.is_some() {
            self.queued_connections.fetch_sub(1, Ordering::Relaxed);
        }
        connection
    }

    pub async fn wait(&mut self) -> Result<(), SessionsManagerClientError> {
        let task = self
            .task
            .take()
            .expect("control-plane task was already awaited");
        task.await?
    }

    pub async fn shutdown(&mut self) -> Result<(), SessionsManagerClientError> {
        self.cancellation.cancel();
        self.wait().await
    }
}

impl Drop for AgentControlPlane {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
