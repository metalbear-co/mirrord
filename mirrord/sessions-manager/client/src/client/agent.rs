use mirrord_protocol_io::{Agent, Connection};
use mirrord_sessions_manager_protocol::{
    AgentIdentity, AssignmentId, ConnectionAssignment, ReplicaId, ServiceScope,
};
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use crate::{
    assignments::DeduplicatingAssignmentSubscriber, client::validate_scope,
    error::SessionsManagerClientError, retry::with_deadline, transport::SessionsManagerTransport,
};

const CONNECTIONS_QUEUE_CAPACITY: usize = 1024;
const QUEUE_WARNING_THRESHOLDS: &[usize] = &[128, 256, 512, 1024];

/// Registers an agent replica with sessions-manager and runs its control plane in the
/// background, handing out each data-plane connection it establishes.
///
/// Dropping the client shuts the control plane down.
pub struct AgentClient {
    receiver: mpsc::Receiver<Connection<Agent>>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<Result<(), SessionsManagerClientError>>>,
}

impl AgentClient {
    /// Spawns the control-plane task, so this must be called within a Tokio runtime.
    ///
    /// `cancellation` shuts the control plane down when fired, as does [`Self::shutdown`].
    pub fn start<T: SessionsManagerTransport>(
        scope: ServiceScope,
        replica_id: ReplicaId,
        transport: T,
        cancellation: impl Into<Option<CancellationToken>>,
    ) -> Result<Self, SessionsManagerClientError> {
        let scope = validate_scope(scope)?;
        let identity = AgentIdentity::new(replica_id);
        let cancellation = cancellation.into().unwrap_or_default();
        let (sender, receiver) = mpsc::channel(CONNECTIONS_QUEUE_CAPACITY);
        let queue = QueueSender { sender };

        let task = tokio::spawn(Self::run(
            transport,
            scope,
            identity,
            queue,
            cancellation.clone(),
        ));

        Ok(Self {
            receiver,
            cancellation,
            task: Some(task),
        })
    }

    /// Shuts the control plane down when `cancellation` fires, by dropping [`Self::run_loop`]
    /// wherever it happens to be suspended.
    ///
    /// This is the only place the token is observed. Everything below is plain polling — dropping
    /// the loop future also drops its [`JoinSet`], which aborts any data-plane upgrade still in
    /// flight.
    async fn run<T: SessionsManagerTransport>(
        transport: T,
        scope: ServiceScope,
        identity: AgentIdentity,
        queue: QueueSender,
        cancellation: CancellationToken,
    ) -> Result<(), SessionsManagerClientError> {
        tokio::select! {
            _ = cancellation.cancelled() => Ok(()),
            result = Self::run_loop(transport, scope, identity, queue) => {
                result
            }
        }
    }

    async fn run_loop<T: SessionsManagerTransport>(
        transport: T,
        scope: ServiceScope,
        identity: AgentIdentity,
        queue: QueueSender,
    ) -> Result<(), SessionsManagerClientError> {
        let mut dataplane_upgrades = JoinSet::new();
        let mut assignments_subscriber =
            DeduplicatingAssignmentSubscriber::new(transport.clone(), scope, identity);

        let result = loop {
            tokio::select! {
                assignment = assignments_subscriber.next() => match assignment {
                    Ok(assignment) => Self::spawn_upgrade_task(
                        &mut dataplane_upgrades,
                        transport.clone(),
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

    fn spawn_upgrade_task<T: SessionsManagerTransport>(
        dataplane_upgrades: &mut JoinSet<(
            AssignmentId,
            Result<Connection<Agent>, SessionsManagerClientError>,
        )>,
        transport: T,
        assignment: ConnectionAssignment,
    ) {
        let assignment_id = assignment.assignment_id.clone();
        dataplane_upgrades.spawn(async move {
            let deadline = tokio::time::Instant::now() + transport.connect_timeout();
            let result = with_deadline(
                Some(deadline),
                transport.connect_data_plane::<Agent>(assignment),
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

impl Drop for AgentClient {
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use futures::{StreamExt, stream};
    use http_body_util::{BodyExt, Empty};
    use hyper::Request;
    use hyper_util::rt::TokioIo;
    use mirrord_operator_websocket::{
        connection::OperatorConnection,
        upgrade::{BoxError, UpgradeContract, connect_ws_direct},
    };
    use mirrord_protocol_io::ProtocolEndpoint;
    use mirrord_sessions_manager_protocol::AssignmentSubscription;
    use tokio::{sync::watch, time::Instant};

    use super::*;
    use crate::control_plane::{ControlPlaneEvent, ControlPlaneEventStream};

    /// Offers the same assignment on every subscription and upgrades it over an in-memory
    /// WebSocket, so the agent control plane runs end to end without a sessions-manager.
    #[derive(Clone)]
    struct MockTransport;

    impl SessionsManagerTransport for MockTransport {
        async fn subscribe_assignments(
            &self,
            _scope: &ServiceScope,
            _subscription: &AssignmentSubscription,
        ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
            let assignment = serde_json::from_value(serde_json::json!({
                "assignment_id": "assignment-1",
                "data_plane_endpoint": "/ws/assignment-1",
                "authorization": "Bearer test",
            }))?;
            let events = stream::iter([Ok(ControlPlaneEvent::Assignment(assignment))])
                .chain(stream::pending());
            let (_activity_tx, activity_rx) = watch::channel(Instant::now());
            Ok(ControlPlaneEventStream::new(Box::pin(events), activity_rx))
        }

        async fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
            &self,
            _assignment: ConnectionAssignment,
        ) -> Result<OperatorConnection<E>, SessionsManagerClientError> {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                let _socket = tokio_tungstenite::accept_async(server_io).await.unwrap();
                std::future::pending::<()>().await;
            });

            let request = Request::get("http://sessions-manager.test/ws/assignment-1")
                .body(Vec::new())
                .unwrap();
            let socket = connect_ws_direct(request, UpgradeContract::Direct, |request| async {
                let (mut sender, connection) =
                    hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                        .await
                        .map_err(|error| Box::new(error) as BoxError)?;
                tokio::spawn(connection.with_upgrades());
                let response = sender
                    .send_request(request.map(|_| Empty::<Bytes>::new()))
                    .await
                    .map_err(|error| Box::new(error) as BoxError)?;
                Ok(response.map(|body| {
                    body.map_err(|error| Box::new(error) as BoxError)
                        .boxed_unsync()
                }))
            })
            .await?;

            Ok(OperatorConnection::new(socket))
        }
    }

    /// The client spawns its control-plane loop and upgrade tasks generically over the transport,
    /// so this also guards that a transport's futures stay `Send`.
    #[tokio::test]
    async fn control_plane_delivers_connection_from_transport() {
        let scope = ServiceScope {
            environment: "test".to_owned(),
            service: "test".to_owned(),
        };
        let mut client =
            AgentClient::start(scope, "replica-a".to_owned().into(), MockTransport, None).unwrap();

        let connection = tokio::time::timeout(Duration::from_secs(10), client.recv())
            .await
            .expect("control plane delivered a connection in time");
        assert!(connection.is_some());

        client.shutdown().await.unwrap();
    }
}
