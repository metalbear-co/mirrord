use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt,
    ops::Not,
    sync::Arc,
};

use futures::{future::Shared, FutureExt};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    connection::{
        http::{
            error_response::MirrordErrorResponse,
            service::{self, ExtractedRequest},
            RedirectedHttp,
        },
        tcp::RedirectedTcp,
        ConnectionInfo, MaybeHttp,
    },
    error::RedirectorTaskError,
    mirror_handle::{MirrorHandle, MirroredTraffic},
    steal_handle::{StealHandle, StolenTraffic},
    tls::IncomingTlsHandlerStore,
    PortRedirector, Redirected,
};

/// A task responsible for redirecting incoming connections.
///
/// # HTTP detection
///
/// This struct runs automatic HTTP detection on **all** incoming connections.
/// Stolen/mirrored traffic is distributed to the handles as HTTP requests, if possible.
///
/// This allows us to smoothly handle persistent incoming HTTP connections -
/// one mirroring/stealing client will not prevent other mirroring clients
/// from receiving the traffic.
///
/// # Redirected connections without subscriptions
///
/// It might be that this task ends up having a redirected connection without any active port
/// subscriptions. In this case, it simply drops the connection.
///
/// It can happen when:
/// 1. The [`PortRedirector`] redirector returns a connection to a port that is no longer subscribed
///    (unlikely race condition).
/// 2. All clients unsubscribed before we finished HTTP detection or extraced an HTTP request.
///
/// The peer should handle this gracefully as a transient network error,
/// and retry the connection.
///
/// # Important
///
/// 1. Has to run in the target's network namespace.
/// 2. Only one instance of this task should run in the agent.
pub struct RedirectorTask<R> {
    /// Implements traffic interception.
    redirector: R,
    /// For accepting TLS connections.
    ///
    /// We need to decode TLS in order to detect HTTP connections
    /// and split the connections into requests.
    tls_handlers: IncomingTlsHandlerStore,
    /// Provides the handles with this task's failure reason.
    ///
    /// Dropping this sender will be seen as a panic in this task.
    error_tx: oneshot::Sender<RedirectorTaskError>,
    /// Allows for receiving steal/mirror requests from the handles.
    external_rx: mpsc::Receiver<RedirectionRequest>,
    /// Cloned and passed to subtasks.
    internal_tx: mpsc::Sender<InternalMessage>,
    /// Allows for receiving updates from subtasks.
    internal_rx: mpsc::Receiver<InternalMessage>,
    /// Keeps track of subscriptions state.
    ports: HashMap<u16, PortState>,
}

impl<R> RedirectorTask<R>
where
    R: 'static + PortRedirector,
    R::Error: Into<Arc<dyn Error + Send + Sync + 'static>>,
{
    /// Capacity of the ([`Self::internal_tx`], [`Self::internal_rx`]) channel.
    const INTERNAL_CHANNEL_CAPACITY: usize = 16;
    /// Capacity of the channel that connects this task with its handles.
    const EXTERNAL_CHANNEL_CAPACITY: usize = 16;
    /// Capacity of channels that connect this task with its handle's subscriptions.
    ///
    /// For each port subscription, a new [`mpsc`] channel is created,
    /// and redirected connections/requests are sent through it.
    ///
    /// Stolen traffic is sent with [`mpsc::Sender::send`], which is blocking.
    /// Mirrored traffic is sent with [`mpsc::Sender::try_send`], which is non-blocking.
    const TRAFFIC_CHANNEL_CAPACITY: usize = 32;

    /// Creates a new instance of this task.
    ///
    /// The task has to be run with [`Self::run`] to start redirecting connections.
    pub fn new(
        redirector: R,
        tls_handlers: IncomingTlsHandlerStore,
    ) -> (Self, StealHandle, MirrorHandle) {
        let (error_tx, error_rx) = oneshot::channel();
        let (external_tx, external_rx) = mpsc::channel(Self::EXTERNAL_CHANNEL_CAPACITY);
        let (internal_tx, internal_rx) = mpsc::channel(Self::INTERNAL_CHANNEL_CAPACITY);

        let task = Self {
            redirector,
            tls_handlers,
            error_tx,
            external_rx,
            internal_tx,
            internal_rx,
            ports: Default::default(),
        };

        let task_error = TaskError(error_rx.shared());

        let steal_handle = StealHandle::new(external_tx.clone(), task_error.clone());
        let mirror_handle = MirrorHandle::new(external_tx.clone(), task_error);

        (task, steal_handle, mirror_handle)
    }

    /// Runs this task without consuming the struct.
    ///
    /// The error should be sent via [`Self::error_tx`].
    async fn run_inner(&mut self) -> Result<(), R::Error> {
        loop {
            tokio::select! {
                conn = self.redirector.next_connection() => {
                    self.handle_connection(conn?);
                },

                message = self.external_rx.recv() => {
                    let Some(message) = message else {
                        // All handles dropped, we can exit.
                        break Ok(());
                    };

                    self.handle_message(message).await?;
                },

                message = self.internal_rx.recv() => {
                    match message.expect("this channel is never closed") {
                        InternalMessage::DeadSteal { port, channel } => {
                            self.handle_dead_steal(port, channel).await?;
                        }

                        InternalMessage::DeadMirror { port, channel } => {
                            self.handle_dead_mirror(port, channel).await?;
                        }

                        InternalMessage::HttpDetectionFinished(conn) => {
                            self.handle_connection_ready(conn).await;
                        }

                        InternalMessage::Request(request, conn_info) => {
                            self.handle_request(request, conn_info).await;
                        }
                    }
                },
            }
        }
    }

    /// Handles a redirected connection coming from [`Self::redirector`].
    ///
    /// Spawns a subtask to run [`MaybeHttp::detect`] on the connection.
    fn handle_connection(&mut self, conn: Redirected) {
        if self.ports.contains_key(&conn.destination.port()).not() {
            tracing::warn!(
                connection = ?conn,
                "Port redirector returned a connection for a port that has no subscriptions, \
                dropping the connection."
            );
            return;
        }

        let tls_handlers = self.tls_handlers.clone();
        let internal_tx = self.internal_tx.clone();

        tokio::spawn(async move {
            let source = conn.source;
            let destination = conn.destination;

            match MaybeHttp::detect(conn, &tls_handlers).await {
                Ok(conn) => {
                    let _ = internal_tx
                        .send(InternalMessage::HttpDetectionFinished(conn))
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %source,
                        %destination,
                        "Failed to determine whether a redirected connection is HTTP.",
                    )
                }
            }
        });
    }

    /// Handles a redirected connection that passed the HTTP detection phase.
    ///
    /// If the connection is not HTTP, passes it to the clients.
    /// If it is HTTP, spawns a subtask that will extract requests
    /// ([`service::extract_requests`]).
    async fn handle_connection_ready(&mut self, conn: MaybeHttp) {
        let Some(state) = self.ports.get(&conn.info.original_destination.port()) else {
            tracing::warn!(
                connection = ?conn,
                "Destination port of a redirected connection is no longer subscribed, \
                dropping the connection."
            );
            return;
        };

        let Some(http_version) = conn.http_version else {
            let mut redirected = RedirectedTcp::new(conn.stream, conn.info);

            for mirror in &state.mirrors {
                if let Ok(permit) = mirror.try_reserve() {
                    permit.send(MirroredTraffic::Tcp(redirected.mirror()));
                }
            }

            if let Some(steal) = &state.steal {
                if let Ok(permit) = steal.reserve().await {
                    permit.send(StolenTraffic::Tcp(redirected));
                } else {
                    redirected.pass_through();
                }
            }

            return;
        };

        let shutdown = state.http_shutdown.clone().cancelled_owned();

        tokio::spawn(service::extract_requests(
            conn.info,
            self.internal_tx.clone(),
            conn.stream,
            http_version,
            shutdown,
        ));
    }

    /// Handles a redirected HTTP request.
    ///
    /// The requests are extracted from redirected connections by the [`service::extract_requests`]
    /// subtask.
    async fn handle_request(&mut self, request: ExtractedRequest, conn_info: ConnectionInfo) {
        let Some(state) = self.ports.get(&conn_info.original_destination.port()) else {
            tracing::warn!(
                connection = ?conn_info,
                method = %request.parts.method,
                uri = %request.parts.uri,
                "Destination port of a redirected HTTP request is no longer subscribed, \
                dropping the request."
            );
            let _ = request.response_tx.send(
                MirrordErrorResponse::new(
                    request.parts.version,
                    "all clients unsubscribed the destination port, try again",
                )
                .into(),
            );
            return;
        };

        let mut redirected = RedirectedHttp::new(conn_info, request);

        for mirror in &state.mirrors {
            if let Ok(permit) = mirror.try_reserve() {
                permit.send(MirroredTraffic::Http(redirected.mirror()));
            };
        }

        if let Some(steal) = &state.steal {
            if let Ok(permit) = steal.reserve().await {
                permit.send(StolenTraffic::Http(redirected));
            } else {
                redirected.pass_through();
            }
        }
    }

    /// Handles a [`RedirectionRequest`] coming from one of this task's handles.
    async fn handle_message(&mut self, message: RedirectionRequest) -> Result<(), R::Error> {
        match message {
            RedirectionRequest::Mirror { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(Self::TRAFFIC_CHANNEL_CAPACITY);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        self.redirector.add_redirection(port).await?;
                        e.insert(PortState {
                            steal: None,
                            mirrors: vec![conn_tx.clone()],
                            http_shutdown: CancellationToken::new(),
                        });
                    }

                    Entry::Occupied(mut e) => {
                        e.get_mut().mirrors.push(conn_tx.clone());
                    }
                }

                let internal_tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    conn_tx.closed().await;
                    let _ = internal_tx
                        .send(InternalMessage::DeadMirror {
                            port,
                            channel: conn_tx,
                        })
                        .await;
                });

                let _ = receiver_tx.send(conn_rx);
            }

            RedirectionRequest::Steal { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(Self::TRAFFIC_CHANNEL_CAPACITY);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        self.redirector.add_redirection(port).await?;
                        e.insert(PortState {
                            steal: Some(conn_tx.clone()),
                            mirrors: Default::default(),
                            http_shutdown: CancellationToken::new(),
                        });
                    }

                    Entry::Occupied(mut e) => {
                        // StealHandle never issues duplicate port subscriptions.
                        // If this entry already has a channel, it must be dead.
                        e.get_mut().steal.replace(conn_tx.clone());
                    }
                }

                let internal_tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    conn_tx.closed().await;
                    let _ = internal_tx
                        .send(InternalMessage::DeadSteal {
                            port,
                            channel: conn_tx,
                        })
                        .await;
                });

                let _ = receiver_tx.send(conn_rx);
            }
        }

        Ok(())
    }

    /// Called when this task's [`StealHandle`] drops its traffic [`mpsc::Receiver`].
    async fn handle_dead_steal(
        &mut self,
        port: u16,
        channel: mpsc::Sender<StolenTraffic>,
    ) -> Result<(), R::Error> {
        let Entry::Occupied(mut e) = self.ports.entry(port) else {
            return Ok(());
        };

        let same_channel = e
            .get()
            .steal
            .as_ref()
            .is_some_and(|c| c.same_channel(&channel));
        if same_channel.not() {
            return Ok(());
        }
        e.get_mut().steal = None;

        if e.get().mirrors.is_empty().not() {
            return Ok(());
        }

        e.remove();

        self.redirector.remove_redirection(port).await?;
        if self.ports.is_empty() {
            self.redirector.cleanup().await?;
        }

        Ok(())
    }

    /// Called when this task's [`MirrorHandle`] drops its traffic [`mpsc::Receiver`].
    async fn handle_dead_mirror(
        &mut self,
        port: u16,
        channel: mpsc::Sender<MirroredTraffic>,
    ) -> Result<(), R::Error> {
        let Entry::Occupied(mut e) = self.ports.entry(port) else {
            return Ok(());
        };

        let position = e
            .get()
            .mirrors
            .iter()
            .position(|c| c.same_channel(&channel));
        let Some(position) = position else {
            return Ok(());
        };
        e.get_mut().mirrors.remove(position);

        if e.get().mirrors.is_empty().not() || e.get().steal.is_some() {
            return Ok(());
        }

        e.remove();

        self.redirector.remove_redirection(port).await?;
        if self.ports.is_empty() {
            self.redirector.cleanup().await?;
        }

        Ok(())
    }

    /// Called when all handles are dropped and this task is about to exit.
    ///
    /// Cleans the redirections in [`Self::redirector`].
    async fn cleanup(&mut self) -> Result<(), R::Error> {
        if self.ports.is_empty() {
            return Ok(());
        }

        for port in std::mem::take(&mut self.ports).into_keys() {
            self.redirector.remove_redirection(port).await?;
        }

        self.redirector.cleanup().await
    }

    /// Runs this task.
    ///
    /// This should be called only in the target's network namespace.
    pub async fn run(mut self) -> Result<(), RedirectorTaskError> {
        let main_result = self.run_inner().await;
        let cleanup_result = self.cleanup().await;

        let result = main_result
            .and(cleanup_result)
            .map_err(|error| RedirectorTaskError::RedirectorError(error.into()));

        if let Err(e) = result.clone() {
            let _ = self.error_tx.send(e);
        }

        result
    }
}

/// Messages produced by [`RedirectorTask`]'s subtasks.
///
/// Passed via ([`RedirectorTask::internal_tx`], [`RedirectorTask::internal_rx`]) channel.
pub(super) enum InternalMessage {
    /// A steal subscription was dropped.
    DeadSteal {
        port: u16,
        channel: mpsc::Sender<StolenTraffic>,
    },
    /// A mirror subscription was dropped.
    DeadMirror {
        port: u16,
        channel: mpsc::Sender<MirroredTraffic>,
    },
    /// HTTP detection finished on a redirected connection.
    HttpDetectionFinished(MaybeHttp),
    /// An HTTP request was extracted from a redirected connection.
    Request(ExtractedRequest, ConnectionInfo),
}

/// State of port's subscriptions.
struct PortState {
    /// Steal subscription.
    steal: Option<mpsc::Sender<StolenTraffic>>,
    /// Mirror subscriptions.
    mirrors: Vec<mpsc::Sender<MirroredTraffic>>,
    /// Used to trigger a graceful shutdown of the [`service::extract_requests`] HTTP server.
    ///
    /// Automatically cancelled when this struct is dropped.
    http_shutdown: CancellationToken,
}

impl Drop for PortState {
    fn drop(&mut self) {
        self.http_shutdown.cancel();
    }
}

/// A request for the [`RedirectorTask`] to start redirecting connections
/// from some port.
///
/// Sent from a [`StealHandle`]/[`MirrorHandle`] to its task.
pub(super) enum RedirectionRequest {
    Steal {
        /// Port to steal.
        port: u16,
        /// Will be used to send the traffic receiver to the [`StealHandle`],
        /// once the [`RedirectorTask`] completes the port steal.
        ///
        /// Using a [`oneshot`] channel here allows the handle to get confirmation
        /// when the subscription is ready.
        receiver_tx: oneshot::Sender<mpsc::Receiver<StolenTraffic>>,
    },

    Mirror {
        /// Port to mirror.
        port: u16,
        /// Will be used to send the traffic receiver to the [`MirrorHandle`],
        /// once the [`RedirectorTask`] completes the port mirror.
        ///
        /// Using a [`oneshot`] channel here allows the handle to get confirmation
        /// when the subscription is ready.
        receiver_tx: oneshot::Sender<mpsc::Receiver<MirroredTraffic>>,
    },
}

/// Can be used to retrieve an error that occurred in the [`RedirectorTask`].
#[derive(Clone)]
pub(super) struct TaskError(Shared<oneshot::Receiver<RedirectorTaskError>>);

impl TaskError {
    /// Resolves when an error occurs in the [`RedirectorTask`].
    pub(super) async fn get(&self) -> RedirectorTaskError {
        self.0
            .clone()
            .await
            .unwrap_or(RedirectorTaskError::Panicked)
    }
}

impl fmt::Debug for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().now_or_never().fmt(f)
    }
}
