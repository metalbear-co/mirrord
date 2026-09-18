use std::sync::Arc;

use mirrord_protocol_io::{Agent, Connection};
use mirrord_sessions_manager_protocol::{AssignmentId, ConnectionAssignment};
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::{
    assignments::DeduplicatingAssignmentSubscriber,
    client::ClientBuilder,
    config::SessionsManagerConfig,
    control_plane::HttpControlPlaneClient,
    credentials::{CredentialProvider, credentials_from_env},
    data_plane::{DataPlaneConnectRequest, DataPlaneTransport, WebSocketDataPlaneTransport},
    error::SessionsManagerClientError,
    retry::with_deadline,
};

const CONNECTIONS_QUEUE_CAPACITY: usize = 1024;
const QUEUE_WARNING_THRESHOLDS: &[usize] = &[128, 256, 512, 1024];

/// Registers an agent replica and starts its sessions-manager control plane.
pub struct AgentClient<T = WebSocketDataPlaneTransport> {
    replica_id: String,
    agent_instance_id: String,
    /// Shuts down the [`AgentControlPlane`] task this client starts.
    cancellation: CancellationToken,
    builder: ClientBuilder<T>,
}

impl AgentClient<WebSocketDataPlaneTransport> {
    pub fn new(
        service: impl Into<String>,
        environment: impl Into<String>,
        replica_id: impl Into<String>,
        cancellation: impl Into<Option<CancellationToken>>,
    ) -> Result<Self, SessionsManagerClientError> {
        Ok(Self {
            replica_id: replica_id.into(),
            agent_instance_id: Uuid::new_v4().to_string(),
            cancellation: cancellation.into().unwrap_or_default(),
            builder: ClientBuilder {
                config: SessionsManagerConfig::new(
                    environment.into(),
                    service.into(),
                    SessionsManagerConfig::base_url_from_env()?,
                )?,
                credentials: credentials_from_env()?,
                transport: WebSocketDataPlaneTransport,
            },
        })
    }
}

impl<T: DataPlaneTransport> AgentClient<T> {
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.builder = self.builder.with_credentials(credentials);
        self
    }

    pub fn with_transport<U: DataPlaneTransport>(self, transport: U) -> AgentClient<U> {
        AgentClient {
            replica_id: self.replica_id,
            agent_instance_id: self.agent_instance_id,
            cancellation: self.cancellation,
            builder: self.builder.with_transport(transport),
        }
    }

    pub fn start_control_plane(self) -> Result<AgentControlPlane, SessionsManagerClientError> {
        let data_plane = DataPlaneContext {
            base_url: self.builder.config.base_url.clone(),
            transport: self.builder.transport,
            credentials: self.builder.credentials.clone(),
        };
        let client = HttpControlPlaneClient::new(&self.builder.config, self.builder.credentials)?;

        Ok(AgentControlPlane::start(
            client,
            self.replica_id,
            self.agent_instance_id,
            self.cancellation,
            data_plane,
        ))
    }
}

/// Carries the shared inputs required to upgrade an assignment's data-plane connection.
struct DataPlaneContext<T> {
    base_url: Url,
    transport: T,
    credentials: Arc<dyn CredentialProvider>,
}

impl<T: Clone> Clone for DataPlaneContext<T> {
    fn clone(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
            transport: self.transport.clone(),
            credentials: self.credentials.clone(),
        }
    }
}

/// Owns the agent's background control-plane task and its established connections.
pub struct AgentControlPlane {
    receiver: mpsc::Receiver<Connection<Agent>>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<Result<(), SessionsManagerClientError>>>,
}

impl AgentControlPlane {
    fn start<T: DataPlaneTransport + 'static>(
        client: HttpControlPlaneClient,
        replica_id: String,
        agent_instance_id: String,
        cancellation: CancellationToken,
        data_plane: DataPlaneContext<T>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(CONNECTIONS_QUEUE_CAPACITY);
        let queue = QueueSender { sender };

        let task = tokio::spawn(Self::run(
            client,
            replica_id,
            agent_instance_id,
            queue,
            cancellation.clone(),
            data_plane,
        ));

        Self {
            receiver,
            cancellation,
            task: Some(task),
        }
    }

    /// Shuts the control plane down when `cancellation` fires, by dropping [`Self::run_loop`]
    /// wherever it happens to be suspended.
    ///
    /// This is the only place the token is observed. Everything below is plain polling — dropping
    /// the loop future also drops its [`JoinSet`], which aborts any data-plane upgrade still in
    /// flight.
    async fn run<T: DataPlaneTransport + 'static>(
        client: HttpControlPlaneClient,
        replica_id: String,
        agent_instance_id: String,
        queue: QueueSender,
        cancellation: CancellationToken,
        data_plane: DataPlaneContext<T>,
    ) -> Result<(), SessionsManagerClientError> {
        tokio::select! {
            _ = cancellation.cancelled() => Ok(()),
            result = Self::run_loop(client, replica_id, agent_instance_id, queue, data_plane) => {
                result
            }
        }
    }

    async fn run_loop<T: DataPlaneTransport + 'static>(
        client: HttpControlPlaneClient,
        replica_id: String,
        agent_instance_id: String,
        queue: QueueSender,
        data_plane: DataPlaneContext<T>,
    ) -> Result<(), SessionsManagerClientError> {
        let mut dataplane_upgrades = JoinSet::new();
        let mut assignments_subscriber =
            DeduplicatingAssignmentSubscriber::new(client, replica_id, agent_instance_id);

        let result = loop {
            tokio::select! {
                assignment = assignments_subscriber.next() => match assignment {
                    Ok(assignment) => Self::spawn_upgrade_task(
                        &mut dataplane_upgrades,
                        data_plane.clone(),
                        assignment,
                    ),
                    Err(error) => break Err(error),
                },
                result = dataplane_upgrades.join_next(), if !dataplane_upgrades.is_empty() => {
                    match result {
                        Some(Ok((assignment_id, Ok(connection)))) => {
                            assignments_subscriber.ack_connected(&assignment_id);
                            match queue.try_send(connection) {
                                Ok(()) => {}
                                Err(QueueSendError::Closed) => {
                                    tracing::debug!(
                                        "sessions-manager control-plane receiver dropped"
                                    );
                                    break Ok(());
                                }
                                Err(QueueSendError::Full) => {
                                    tracing::error!(
                                        queue_size = CONNECTIONS_QUEUE_CAPACITY,
                                        "sessions-manager data-plane connections queue full, retrying assignment"
                                    );
                                    assignments_subscriber.retry_assignment(&assignment_id).await;
                                }
                            }
                        }
                        Some(Ok((assignment_id, Err(error)))) => {
                            tracing::warn!(%assignment_id, %error, "failed to connect sessions-manager data plane");
                            assignments_subscriber.retry_assignment(&assignment_id).await;
                        }
                        Some(Err(error)) if !error.is_cancelled() => {
                            tracing::warn!(%error, "sessions-manager data-plane task failed");
                        }
                        _ => {}
                    }
                }
            }
        };

        dataplane_upgrades.abort_all();
        while dataplane_upgrades.join_next().await.is_some() {}
        result
    }

    fn spawn_upgrade_task<T: DataPlaneTransport + 'static>(
        dataplane_upgrades: &mut JoinSet<(
            AssignmentId,
            Result<Connection<Agent>, SessionsManagerClientError>,
        )>,
        data_plane: DataPlaneContext<T>,
        assignment: ConnectionAssignment,
    ) {
        let DataPlaneContext {
            base_url,
            transport,
            credentials,
        } = data_plane;
        let assignment_id = assignment.assignment_id.clone();
        dataplane_upgrades.spawn(async move {
            let deadline = tokio::time::Instant::now() + transport.connect_timeout();
            let result = with_deadline(
                Some(deadline),
                transport.connect::<Agent>(DataPlaneConnectRequest {
                    control_plane_url: base_url,
                    assignment,
                    credentials,
                }),
            )
            .await
            .flatten()
            .map(Connection::from_channel);
            (assignment_id, result)
        });

        let depth = dataplane_upgrades.len();
        warn_if_threshold_reached(depth, "upgrades are accumulating");
    }

    pub async fn recv(&mut self) -> Option<Connection<Agent>> {
        self.receiver.recv().await
    }

    pub async fn wait(&mut self) -> Result<(), SessionsManagerClientError> {
        let task = self
            .task
            .take()
            .ok_or(SessionsManagerClientError::AlreadyShutdown)?;
        task.await.map_err(|e| {
            if e.is_panic() {
                SessionsManagerClientError::TaskPanicked
            } else {
                SessionsManagerClientError::TaskCancelled
            }
        })?
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

fn warn_if_threshold_reached(depth: usize, context: &str) {
    if QUEUE_WARNING_THRESHOLDS.contains(&depth) {
        tracing::warn!(depth, "sessions-manager data-plane {}", context);
    }
}

enum QueueSendError {
    Full,
    Closed,
}

/// Sends established data-plane connections to the control-plane consumer.
struct QueueSender {
    sender: mpsc::Sender<Connection<Agent>>,
}

impl QueueSender {
    fn try_send(&self, connection: Connection<Agent>) -> Result<(), QueueSendError> {
        match self.sender.try_send(connection) {
            Ok(()) => {
                // `capacity()` is permits still free, so what's occupied is what's queued.
                let depth = self.sender.max_capacity() - self.sender.capacity();
                warn_if_threshold_reached(depth, "connections queue at capacity");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(QueueSendError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(QueueSendError::Closed),
        }
    }
}
