//! Home for [`StolenConnections`] - manager for connections that were stolen based on active port
//! subscriptions.

use std::{collections::HashMap, fmt, io, net::SocketAddr, time::Duration};

use hyper::{body::Incoming, Request, Response};
use mirrord_protocol::{tcp::NewTcpConnection, ConnectionId, Port, RequestId};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{self, error::SendError, Receiver, Sender},
    task::JoinSet,
};
use tracing::Level;

use self::{filtered::DynamicBody, unfiltered::UnfilteredStealTask};
use super::{http::DefaultReversibleStream, subscriptions::PortSubscription};
use crate::{
    http::HttpVersion,
    metrics::{STEAL_FILTERED_CONNECTION_SUBSCRIPTION, STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION},
    steal::connections::filtered::FilteredStealTask,
    util::ClientId,
};

mod filtered;
mod unfiltered;

/// Messages consumed by [`StolenConnections`] manager. Targeted at a specific [`StolenConnection`].
pub enum ConnectionMessageIn {
    /// Client sent some bytes.
    ///
    /// This variant translates to
    /// [`LayerTcpSteal::Data`](mirrord_protocol::tcp::LayerTcpSteal::Data) coming from the layer.
    Raw { client_id: ClientId, data: Vec<u8> },
    /// Client provided an HTTP response to a stolen request.
    ///
    /// This variant translates to
    /// [`LayerTcpSteal::HttpResponse`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponse)
    /// or [`LayerTcpSteal::HttpResponseFramed`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponseFramed) coming from the layer.
    Response {
        client_id: ClientId,
        request_id: RequestId,
        response: Response<DynamicBody>,
    },
    /// Client failed to provide an HTTP response to a stolen request.
    ///
    /// This variant does not translate to any
    /// [`LayerTcpSteal`](mirrord_protocol::tcp::LayerTcpSteal) message. It is used only
    /// internally, to notify [`StolenConnections`] that the client failed to provide a response
    /// (e.g. client abruptly disconnected from the agent).
    ResponseFailed {
        client_id: ClientId,
        request_id: RequestId,
    },
    /// Client unsubscribed the connection.
    ///
    /// This variant translates to
    /// [`LayerTcpSteal::ConnectionUnsubscribe](mirrord_protocol::tcp::LayerTcpSteal::ConnectionUnsubscribe)
    /// coming from the layer.
    Unsubscribed { client_id: ClientId },
}

impl fmt::Debug for ConnectionMessageIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("ConnectionMessageIn");

        match self {
            Self::Raw { client_id, data } => {
                debug_struct.field("type", &"Raw");
                debug_struct.field("client_id", client_id);
                debug_struct.field("data_len", &data.len());
            }
            Self::Response {
                client_id,
                request_id,
                response,
            } => {
                debug_struct.field("type", &"Response");
                debug_struct.field("client_id", client_id);
                debug_struct.field("request_id", request_id);
                debug_struct.field("status_code", &response.status());
            }
            Self::ResponseFailed {
                client_id,
                request_id,
            } => {
                debug_struct.field("type", &"ResponseFailed");
                debug_struct.field("client_id", client_id);
                debug_struct.field("request_id", request_id);
            }
            Self::Unsubscribed { client_id } => {
                debug_struct.field("type", &"Unsubscribed");
                debug_struct.field("client_id", client_id);
            }
        }

        debug_struct.finish()
    }
}

impl ConnectionMessageIn {
    /// Returns [`ClientId`] of the client that triggered this message.
    pub fn client_id(&self) -> ClientId {
        match self {
            Self::Raw { client_id, .. } => *client_id,
            Self::Response { client_id, .. } => *client_id,
            Self::ResponseFailed { client_id, .. } => *client_id,
            Self::Unsubscribed { client_id } => *client_id,
        }
    }
}

/// Update from some [`StolenConnection`] managed by [`StolenConnections`].
///
/// # Note
///
/// When filtered steal is in use, one [`StolenConnection`] can produce updates targeted at multiple
/// clients.
pub enum ConnectionMessageOut {
    /// The client received some data from the connection.
    ///
    /// This variant translates to [`DaemonTcp::Data`](mirrord_protocol::tcp::DaemonTcp::Data)
    /// being sent to the layer.
    Raw {
        client_id: ClientId,
        connection_id: ConnectionId,
        data: Vec<u8>,
    },
    /// An incoming HTTP request was matched with the client's
    /// [`HttpFilter`](super::http::HttpFilter).
    ///
    /// This variant translates to
    /// [`DaemonTcp::HttpRequest`](mirrord_protocol::tcp::DaemonTcp::HttpRequest) or
    /// [`DaemonTcp::HttpRequestFramed`](mirrord_protocol::tcp::DaemonTcp::HttpRequestFramed) being
    /// sent to the layer.
    Request {
        client_id: ClientId,
        connection_id: ConnectionId,
        request: Request<Incoming>,
        id: RequestId,
        port: Port,
    },
    /// Subscribed the client to a new TCP connection.
    ///
    /// This variant translates to
    /// [`DaemonTcp::NewConnection`](mirrord_protocol::tcp::DaemonTcp::NewConnection).
    ///
    /// # Note
    ///
    /// This should always be the first message that the client receives from an unfiltered
    /// connection. Only [`ConnectionMessageOut::Raw`] and [`ConnectionMessageOut::Closed`]
    /// should follow.
    SubscribedTcp {
        client_id: ClientId,
        connection: NewTcpConnection,
    },
    /// Subscribed the client to a new filtered HTTP connection.
    ///
    /// This variant **does not** translate to any [`DaemonTcp`](mirrord_protocol::tcp::DaemonTcp)
    /// message. It is used only internally, to indicate that this client's
    /// [`HttpFilter`](super::http::HttpFilter) matched a request for the first time in this
    /// connection ([`ConnectionMessageOut::Request`] should follow immediately).
    ///
    /// # Note
    ///
    /// This should always be the first message that the client receives from a filtered
    /// connection. [`ConnectionMessageOut::Request`] should follow, until the connection
    /// closes or upgrades. If it closes for the given client, [`ConnectionMessageOut::Closed`]
    /// should be received. It if upgrades for the given client, [`ConnectionMessageOut::Raw`]
    /// should follow.
    SubscribedHttp {
        client_id: ClientId,
        connection_id: ConnectionId,
    },
    /// The connection was closed for the given client.
    ///
    /// This variant translates to [`DaemonTcp::Close`](mirrord_protocol::tcp::DaemonTcp::Close).
    Closed {
        client_id: ClientId,
        connection_id: ConnectionId,
    },
}

impl fmt::Debug for ConnectionMessageOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("ConnectionMessageOut");

        match self {
            Self::Raw {
                client_id,
                connection_id,
                data,
            } => {
                debug_struct.field("type", &"Raw");
                debug_struct.field("connection_id", connection_id);
                debug_struct.field("client_id", client_id);
                debug_struct.field("data_len", &data.len());
            }
            Self::Request {
                client_id,
                connection_id,
                request,
                id,
                port,
            } => {
                debug_struct.field("type", &"Request");
                debug_struct.field("connection_id", connection_id);
                debug_struct.field("client_id", client_id);
                debug_struct.field("request_id", id);
                debug_struct.field("port", port);
                debug_struct.field("method", request.method());
                debug_struct.field("uri", request.uri());
            }
            Self::SubscribedTcp {
                client_id,
                connection,
            } => {
                debug_struct.field("type", &"SubscribedTcp");
                debug_struct.field("connection_id", &connection.connection_id);
                debug_struct.field("client_id", client_id);
                debug_struct.field("connection", connection);
            }
            Self::SubscribedHttp {
                client_id,
                connection_id,
            } => {
                debug_struct.field("type", &"SubscribedHttp");
                debug_struct.field("connection_id", connection_id);
                debug_struct.field("client_id", client_id);
            }
            Self::Closed {
                client_id,
                connection_id,
            } => {
                debug_struct.field("type", &"Closed");
                debug_struct.field("connection_id", connection_id);
                debug_struct.field("client_id", client_id);
            }
        }

        debug_struct.finish()
    }
}

/// A set of [`StolenConnection`]s, each managed in its own [`tokio::task`].
///
/// Connection stealing logic is implemented in
/// [`PortSubscriptions`](super::subscriptions::PortSubscriptions). This struct is responsible only
/// for handling IO on the [`StolenConnection`]s.
pub struct StolenConnections {
    /// [`ConnectionId`] that will be assigned to the next [`StolenConnection`].
    next_connection_id: u64,

    /// For sending [`ConnectionMessageIn`] to per-connection [`tokio::task`]s.
    connection_txs: HashMap<ConnectionId, Sender<ConnectionMessageIn>>,
    /// For joining per-connection [`tokio::task`]s.
    /// When the task is finished, its sender in [`Self::connection_txs`] should be removed.
    tasks: JoinSet<ConnectionId>,

    /// Sender of the [`mpsc`] channel shared between all per-connection [`tokio::task`]s.
    ///
    /// Each task receives a clone of this sender and uses it to send [`ConnectionMessageOut`]s,
    /// which are then returned from [`Self::wait`].
    main_tx: Sender<ConnectionMessageOut>,
    /// Receiver of the [`mpsc`] channel shared between all per-connection [`tokio::task`]s.
    ///
    /// Allows for polling updates from all spawned tasks in [`Self::wait`].
    main_rx: Receiver<ConnectionMessageOut>,
}

impl StolenConnections {
    /// Capacity for ([`Self::main_tx`], [`Self::main_tx`]) channel shared between all
    /// per-connection [`tokio::task`]s.
    const MAIN_CHANNEL_CAPACITY: usize = 1024;

    /// Capacity for per-task channels for sending [`ConnectionMessageIn`] to the tasks.
    const TASK_IN_CHANNEL_CAPACITY: usize = 16;

    /// Creates a new empty set of [`StolenConnection`]s.
    pub fn with_capacity(capacity: usize) -> Self {
        let (main_tx, main_rx) = mpsc::channel(Self::MAIN_CHANNEL_CAPACITY);

        Self {
            next_connection_id: 0,

            connection_txs: HashMap::with_capacity(capacity),
            tasks: Default::default(),

            main_tx,
            main_rx,
        }
    }

    /// Adds the given [`StolenConnection`] to this set. Spawns a new [`tokio::task`] that will
    /// manage it.
    #[tracing::instrument(level = Level::TRACE, name = "manage_stolen_connection", skip(self))]
    pub fn manage(&mut self, connection: StolenConnection) {
        let connection_id = self.next_connection_id;
        self.next_connection_id += 1;

        let (task_tx, task_rx) = mpsc::channel(Self::TASK_IN_CHANNEL_CAPACITY);
        let main_tx = self.main_tx.clone();

        tracing::trace!(connection_id, "Spawning connection task");
        self.tasks.spawn(async move {
            let task = ConnectionTask {
                connection_id,
                connection,
                tx: main_tx,
                rx: task_rx,
            };

            match task.run().await {
                Ok(()) => {
                    tracing::trace!(connection_id, "Connection task finished");
                }
                Err(error) => {
                    tracing::trace!(connection_id, ?error, "Connection task failed");
                }
            }

            connection_id
        });

        self.connection_txs.insert(connection_id, task_tx);
    }

    /// Sends the given [`ConnectionMessageIn`] to the task responsible for the connection with the
    /// given [`ConnectionId`]. If the task is not found or dead, does nothing.
    #[tracing::instrument(level = "trace", name = "send_message_to_connection_task", skip(self))]
    pub async fn send(&self, connection_id: ConnectionId, message: ConnectionMessageIn) {
        let Some(tx) = self.connection_txs.get(&connection_id) else {
            tracing::trace!(
                connection_id,
                "Cannot send message, connection task not found"
            );

            return;
        };

        if tx.send(message).await.is_err() {
            tracing::trace!(
                connection_id,
                "Cannot send message, connection task is dead"
            );
        }
    }

    /// Waits for an update from one of the connection tasks in this set.
    ///
    /// # Note
    ///
    /// If this set is empty, this method blocks forever.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel safe.
    pub async fn wait(&mut self) -> ConnectionMessageOut {
        loop {
            tokio::select! {
                Some(task_res) = self.tasks.join_next() => match task_res {
                    Ok(connection_id) => {
                        self.connection_txs.remove(&connection_id);
                    },

                    Err(error) => {
                        tracing::error!(?error, "Failed to join connection task");
                    },
                },

                message = self.main_rx.recv() => break message.expect("this channel is never closed"),
            }
        }
    }
}

/// An incoming TCP connection stolen by an active [`PortSubscription`].
pub struct StolenConnection {
    /// Raw OS stream.
    pub stream: TcpStream,
    /// Source of this connection.
    /// Will be used instead of [`TcpStream::peer_addr`] of [`Self::stream`].
    pub source: SocketAddr,
    /// Destination of this connection.
    /// Will be used instead of [`TcpStream::local_addr`] of [`Self::stream`].
    pub destination: SocketAddr,
    /// Subscription that triggered the steal.
    pub port_subscription: PortSubscription,
}

impl fmt::Debug for StolenConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StolenConnection")
            .field("source", &self.source)
            .field("destination", &self.destination)
            .field(
                "filtered",
                &matches!(self.port_subscription, PortSubscription::Filtered(..)),
            )
            .finish()
    }
}

/// Errors that can occur in [`ConnectionTask`] while managing a [`StolenConnection`].
/// These are not propagated outside [`StolenConnections`] set. This type is only introduced to
/// simplify code with the `?` operator.
#[derive(Error, Debug)]
enum ConnectionTaskError {
    #[error("failed to send message to parent - channel closed")]
    SendError,
    #[error("failed to receive message from parent - channel closed")]
    RecvError,
    #[error("failed to execute io on a TCP connection: {0}")]
    IoError(#[from] io::Error),
    #[error("tokio task spawned for polling an HTTP connection panicked")]
    HttpConnectionTaskPanicked,
}

impl<T> From<SendError<T>> for ConnectionTaskError {
    fn from(_: SendError<T>) -> Self {
        Self::SendError
    }
}

/// Task responsible for managing a single [`StolenConnection`].
/// Run by [`StolenConnections`] in a separate [`tokio::task`].
struct ConnectionTask {
    /// Id of the managed connection, assigned by [`StolenConnections`].
    connection_id: ConnectionId,
    /// Managed connection.
    connection: StolenConnection,
    /// Receiving end of the exclusive channel between this task and [`StolenConnections`].
    rx: Receiver<ConnectionMessageIn>,
    /// Sending end of the channel shared between all [`ConnectionTask`]s and [`StolenConnections`]
    /// set.
    tx: Sender<ConnectionMessageOut>,
}

impl ConnectionTask {
    /// Timeout for detemining if the owned [`StolenConnection::stream`] is an incoming HTTP
    /// connection. Used to pick between [`UnfilteredStealTask`] and [`FilteredStealTask`] when
    /// [`PortSubscription::Filtered`] is in use.
    const HTTP_DETECTION_TIMEOUT: Duration = Duration::from_secs(10);

    /// Runs this task until the connection is closed.
    ///
    /// # Note
    ///
    /// This task is responsible for **always** sending [`ConnectionMessageOut::Closed`] to
    /// interested stealer clients, even when an error has occurred.
    async fn run(mut self) -> Result<(), ConnectionTaskError> {
        match self.connection.port_subscription {
            PortSubscription::Unfiltered(client_id) => {
                self.tx
                    .send(ConnectionMessageOut::SubscribedTcp {
                        client_id,
                        connection: NewTcpConnection {
                            connection_id: self.connection_id,
                            remote_address: self.connection.source.ip(),
                            destination_port: self.connection.destination.port(),
                            source_port: self.connection.source.port(),
                            local_address: self.connection.stream.local_addr()?.ip(),
                        },
                    })
                    .await?;

                let task = UnfilteredStealTask {
                    connection_id: self.connection_id,
                    client_id,
                    stream: self.connection.stream,
                };

                STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.inc();

                task.run(self.tx, &mut self.rx).await
            }

            PortSubscription::Filtered(filters) => {
                let mut stream = DefaultReversibleStream::read_header(
                    self.connection.stream,
                    Self::HTTP_DETECTION_TIMEOUT,
                )
                .await?;

                let Some(http_version) = HttpVersion::new(stream.get_header()) else {
                    tracing::trace!(
                        "No HTTP version detected, proxying the connection transparently"
                    );

                    let mut outgoing_io = TcpStream::connect(self.connection.destination).await?;
                    tokio::io::copy_bidirectional(&mut stream, &mut outgoing_io).await?;

                    return Ok(());
                };

                tracing::trace!(?http_version, "Detected HTTP version");

                let task = FilteredStealTask::new(
                    self.connection_id,
                    filters,
                    self.connection.destination,
                    http_version,
                    stream,
                );

                task.run(self.tx.clone(), &mut self.rx).await
            }
        }
    }
}
