//! Home for [`StolenConnections`] - manager for connections that were stolen based on active port
//! subscriptions.

use std::{
    collections::HashMap,
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use hyper::{body::Incoming, Request, Response};
use mirrord_protocol::{
    tcp::{HttpRequestMetadata, IncomingTrafficTransportType, NewTcpConnectionV1},
    ConnectionId, LogMessage, RequestId,
};
use original_destination::OriginalDestination;
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{self, error::SendError, Receiver, Sender},
    task::JoinSet,
};
use tracing::Level;

use self::{filtered::DynamicBody, unfiltered::UnfilteredStealTask};
use super::{
    http::DefaultReversibleStream,
    subscriptions::PortSubscription,
    tls::{self, error::StealTlsSetupError, StealTlsHandlerStore},
};
use crate::{
    http::HttpVersion, incoming::RedirectedConnection,
    metrics::STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION,
    steal::connections::filtered::FilteredStealTask, util::ClientId,
};

mod filtered;
mod original_destination;
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
        metadata: HttpRequestMetadata,
        transport: IncomingTrafficTransportType,
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
        connection: NewTcpConnectionV1,
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
    /// The client recevied a log message from the connection.
    ///
    /// This variant translates to [`LogMessage`].
    LogMessage {
        client_id: ClientId,
        connection_id: ConnectionId,
        message: LogMessage,
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
                metadata,
                transport,
            } => {
                debug_struct.field("type", &"Request");
                debug_struct.field("connection_id", connection_id);
                debug_struct.field("client_id", client_id);
                debug_struct.field("request_id", id);
                debug_struct.field("metadata", metadata);
                debug_struct.field("transport", transport);
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
            Self::LogMessage {
                client_id,
                connection_id,
                message,
            } => {
                debug_struct.field("type", &"LogMessage");
                debug_struct.field("connection_id", connection_id);
                debug_struct.field("client_id", client_id);
                debug_struct.field("message", message);
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

    /// For stealing HTTPS with filters.
    tls_handler_store: Option<StealTlsHandlerStore>,
}

impl StolenConnections {
    /// Capacity for ([`Self::main_tx`], [`Self::main_tx`]) channel shared between all
    /// per-connection [`tokio::task`]s.
    const MAIN_CHANNEL_CAPACITY: usize = 1024;

    /// Capacity for per-task channels for sending [`ConnectionMessageIn`] to the tasks.
    const TASK_IN_CHANNEL_CAPACITY: usize = 16;

    /// Creates a new empty set of [`StolenConnection`]s.
    pub fn new(capacity: usize, tls_handler_store: Option<StealTlsHandlerStore>) -> Self {
        let (main_tx, main_rx) = mpsc::channel(Self::MAIN_CHANNEL_CAPACITY);

        Self {
            next_connection_id: 0,

            connection_txs: HashMap::with_capacity(capacity),
            tasks: Default::default(),

            main_tx,
            main_rx,

            tls_handler_store,
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
        let tls_handler_store = self.tls_handler_store.clone();

        tracing::trace!(connection_id, "Spawning connection task");
        self.tasks.spawn(async move {
            let task = ConnectionTask {
                connection_id,
                connection,
                tx: main_tx,
                rx: task_rx,
                tls_handler_store,
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
    pub connection: RedirectedConnection,
    /// Subscription that triggered the steal.
    pub port_subscription: PortSubscription,
}

impl fmt::Debug for StolenConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StolenConnection")
            .field("connection", &self.connection)
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
    #[error("failed to prepare mirrord-agent TLS handler")]
    TlsSetupError(#[from] StealTlsSetupError),
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
    /// For stealing HTTPS with filters.
    tls_handler_store: Option<StealTlsHandlerStore>,
}

impl ConnectionTask {
    /// Timeout for detemining if the owned [`StolenConnection`] is an incoming HTTP
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
        let StolenConnection {
            connection,
            port_subscription,
        } = self.connection;

        let mut destination = connection.destination();
        let localhost = if destination.is_ipv6() {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        // If we use the original IP we would go through prerouting and hit a loop.
        // localhost should always work.
        destination.set_ip(localhost);

        let source = connection.source();

        let filters = match port_subscription {
            PortSubscription::Unfiltered(client_id) => {
                self.tx
                    .send(ConnectionMessageOut::SubscribedTcp {
                        client_id,
                        connection: NewTcpConnectionV1 {
                            connection_id: self.connection_id,
                            remote_address: source.ip(),
                            source_port: source.port(),
                            destination_port: destination.port(),
                            local_address: connection.local_addr()?.ip(),
                        },
                    })
                    .await?;

                return UnfilteredStealTask::new(self.connection_id, client_id, connection)
                    .run(self.tx, &mut self.rx)
                    .await;
            }

            PortSubscription::Filtered(filters) => filters,
        };

        let tls_handler = match self.tls_handler_store {
            Some(store) => store.get(destination.port()).await?,
            None => None,
        };

        let Some(tls_handler) = tls_handler else {
            let mut stream =
                DefaultReversibleStream::read_header(connection, Self::HTTP_DETECTION_TIMEOUT)
                    .await?;

            let Some(http_version) = HttpVersion::new(stream.get_header()) else {
                tracing::trace!("No HTTP version detected, proxying the connection transparently");

                let mut outgoing_io = TcpStream::connect(destination).await?;
                tokio::io::copy_bidirectional(&mut stream, &mut outgoing_io).await?;

                return Ok(());
            };

            tracing::trace!(?http_version, "Detected HTTP version");

            let task = FilteredStealTask::new(
                self.connection_id,
                filters,
                OriginalDestination::new(destination, None),
                source,
                http_version,
                stream,
            );

            return task.run(self.tx.clone(), &mut self.rx).await;
        };

        let mut tls_stream = tls_handler
            .acceptor()
            .accept(connection)
            .await
            .map(Box::new)?; // size of `TlsStream` exceeds 1kb, let's box it

        let http_version = match tls_stream.get_ref().1.alpn_protocol() {
            Some(tls::HTTP_2_ALPN_NAME) => {
                tracing::debug!(
                    original_destination = %destination,
                    "Stolen TLS connection was upgraded to HTTP/2 with ALPN"
                );

                HttpVersion::V2
            }
            Some(tls::HTTP_1_1_ALPN_NAME) => {
                tracing::debug!(
                    original_destination = %destination,
                    "Stolen TLS connection was upgraded to HTTP/1.1 with ALPN"
                );

                HttpVersion::V1
            }
            Some(tls::HTTP_1_0_ALPN_NAME) => {
                tracing::debug!(
                    original_destination = %destination,
                    "Stolen TLS connection was upgraded to HTTP/1.0 with ALPN"
                );

                HttpVersion::V1
            }
            Some(other) => {
                tracing::info!(
                    protocol = %String::from_utf8_lossy(other),
                    original_destination = %destination,
                    "Stolen TLS connection was upgraded with ALPN to a non-HTTP protocol. \
                    Proxying data to the original destination.",
                );

                let outgoing_io = TcpStream::connect(destination).await?;
                let connector = tls_handler.connector(tls_stream.get_ref().1);
                let mut outgoing_io = connector
                    .connect(destination.ip(), None, outgoing_io)
                    .await?;

                tokio::io::copy_bidirectional(&mut tls_stream, &mut outgoing_io).await?;

                return Ok(());
            }
            None => {
                let mut stream =
                    DefaultReversibleStream::read_header(tls_stream, Self::HTTP_DETECTION_TIMEOUT)
                        .await?;

                let Some(http_version) = HttpVersion::new(stream.get_header()) else {
                    tracing::trace!(
                        "No HTTP version detected, proxying the connection transparently"
                    );

                    let outgoing_io = TcpStream::connect(destination).await?;
                    let connector = tls_handler.connector(stream.inner().get_ref().1);
                    let mut outgoing_io = connector
                        .connect(destination.ip(), None, outgoing_io)
                        .await?;

                    tokio::io::copy_bidirectional(&mut stream, &mut outgoing_io).await?;

                    return Ok(());
                };

                let connector = tls_handler.connector(stream.inner().get_ref().1);
                let task = FilteredStealTask::new(
                    self.connection_id,
                    filters,
                    OriginalDestination::new(destination, Some(connector)),
                    source,
                    http_version,
                    stream,
                );

                return task.run(self.tx.clone(), &mut self.rx).await;
            }
        };

        let connector = tls_handler.connector(tls_stream.get_ref().1);
        let task = FilteredStealTask::new(
            self.connection_id,
            filters,
            OriginalDestination::new(destination, Some(connector)),
            source,
            http_version,
            tls_stream,
        );

        return task.run(self.tx.clone(), &mut self.rx).await;
    }
}
