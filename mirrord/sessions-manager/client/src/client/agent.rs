use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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
    assignments::AgentAssignmentSubscriber,
    config::SessionsManagerConfig,
    control_plane::HttpControlPlaneClient,
    credentials::{CredentialProvider, NoCredentials},
    data_plane::{DataPlaneConnectRequest, DataPlaneTransport, WebSocketDataPlaneTransport},
    environment::sessions_manager_environment,
    error::SessionsManagerClientError,
    retry::run_interruptible,
};

const CONNECTIONS_QUEUE_CAPACITY: usize = 1024;
const QUEUE_WARNING_THRESHOLDS: &[usize] = &[128, 256, 512, 1024];

pub struct AgentClient<T = WebSocketDataPlaneTransport> {
    replica_id: String,
    instance_id: String,
    config: SessionsManagerConfig,
    credentials: Arc<dyn CredentialProvider>,
    cancellation: CancellationToken,
    transport: T,
}

impl AgentClient<WebSocketDataPlaneTransport> {
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
            instance_id: Uuid::new_v4().to_string(),
            transport: WebSocketDataPlaneTransport,
        })
    }
}

impl<T: DataPlaneTransport> AgentClient<T> {
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    pub fn with_transport<U: DataPlaneTransport>(self, transport: U) -> AgentClient<U> {
        AgentClient {
            config: self.config,
            replica_id: self.replica_id,
            cancellation: self.cancellation,
            credentials: self.credentials,
            instance_id: self.instance_id,
            transport,
        }
    }

    pub fn start_control_plane(self) -> Result<AgentControlPlane, SessionsManagerClientError> {
        let client = HttpControlPlaneClient::new(&self.config, self.credentials)?;

        Ok(AgentControlPlane::start(
            client,
            self.replica_id,
            self.instance_id,
            self.config.base_url,
            self.cancellation,
            self.transport,
        ))
    }
}

pub struct AgentControlPlane {
    receiver: mpsc::Receiver<Connection<Agent>>,
    pending: Arc<AtomicUsize>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<Result<(), SessionsManagerClientError>>>,
}

impl AgentControlPlane {
    fn start<T: DataPlaneTransport + 'static>(
        client: HttpControlPlaneClient,
        replica_id: String,
        instance_id: String,
        data_plane_base_url: Url,
        cancellation: CancellationToken,
        transport: T,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(CONNECTIONS_QUEUE_CAPACITY);
        let pending = Arc::new(AtomicUsize::new(0));
        let queue = QueueSender {
            sender,
            pending: pending.clone(),
        };

        let task = tokio::spawn(Self::run(
            client,
            replica_id,
            instance_id,
            data_plane_base_url,
            queue,
            cancellation.clone(),
            transport,
        ));

        Self {
            receiver,
            pending,
            cancellation,
            task: Some(task),
        }
    }

    async fn run<T: DataPlaneTransport + 'static>(
        client: HttpControlPlaneClient,
        replica_id: String,
        instance_id: String,
        data_plane_base_url: Url,
        queue: QueueSender,
        cancellation: CancellationToken,
        transport: T,
    ) -> Result<(), SessionsManagerClientError> {
        let mut dataplane_upgrades = JoinSet::new();
        let mut assignments_subscriber =
            AgentAssignmentSubscriber::new(client, replica_id, instance_id, cancellation.clone());

        let result = loop {
            tokio::select! {
                _ = cancellation.cancelled() => break Ok(()),
                assignment = assignments_subscriber.next() => match assignment {
                    Some(Ok(assignment)) => Self::spawn_upgrade_task(
                        &mut dataplane_upgrades,
                        &data_plane_base_url,
                        &cancellation,
                        transport.clone(),
                        assignment,
                    ),
                    Some(Err(error)) => break Err(error),
                    None => break Ok(()),
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
                                    if Self::handle_retry(&mut assignments_subscriber, &assignment_id)
                                        .await?
                                    {
                                        break Ok(());
                                    }
                                }
                            }
                        }
                        Some(Ok((assignment_id, Err(error)))) => {
                            tracing::warn!(%assignment_id, %error, "failed to connect sessions-manager data plane");
                            if Self::handle_retry(&mut assignments_subscriber, &assignment_id)
                                .await?
                            {
                                break Ok(());
                            }
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

    async fn handle_retry(
        assignments_subscriber: &mut AgentAssignmentSubscriber,
        assignment_id: &AssignmentId,
    ) -> Result<bool, SessionsManagerClientError> {
        match assignments_subscriber.retry(assignment_id).await {
            Ok(()) => Ok(false),
            Err(SessionsManagerClientError::Cancelled) => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn spawn_upgrade_task<T: DataPlaneTransport + 'static>(
        dataplane_upgrades: &mut JoinSet<(
            AssignmentId,
            Result<Connection<Agent>, SessionsManagerClientError>,
        )>,
        data_plane_base_url: &Url,
        cancellation: &CancellationToken,
        transport: T,
        assignment: ConnectionAssignment,
    ) {
        let base_url = data_plane_base_url.clone();
        let cancellation = cancellation.clone();
        let assignment_id = assignment.assignment_id.clone();
        dataplane_upgrades.spawn(async move {
            let deadline = tokio::time::Instant::now() + transport.connect_timeout();
            let result = run_interruptible(
                &cancellation,
                Some(deadline),
                transport.connect(DataPlaneConnectRequest {
                    control_plane_url: base_url,
                    assignment,
                }),
            )
            .await
            .flatten();
            (assignment_id, result)
        });

        let depth = dataplane_upgrades.len();
        warn_if_threshold_reached(depth, "upgrades are accumulating");
    }

    pub async fn recv(&mut self) -> Option<Connection<Agent>> {
        let connection = self.receiver.recv().await;
        if connection.is_some() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
        }
        connection
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

/// Sending half of the connections queue, shared between [`AgentControlPlane::run`] and its
/// [`AgentControlPlane::recv`]er so both sides agree on how many connections are pending.
#[derive(Clone)]
struct QueueSender {
    sender: mpsc::Sender<Connection<Agent>>,
    pending: Arc<AtomicUsize>,
}

impl QueueSender {
    fn try_send(&self, connection: Connection<Agent>) -> Result<(), QueueSendError> {
        match self.sender.try_send(connection) {
            Ok(()) => {
                let depth = self.pending.fetch_add(1, Ordering::Relaxed) + 1;
                warn_if_threshold_reached(depth, "connections queue at capacity");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(QueueSendError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(QueueSendError::Closed),
        }
    }
}
