use std::{collections::HashMap, fmt, io, net::SocketAddr, time::Duration};

use hyper::{body::Incoming, Request, Response};
use mirrord_protocol::{tcp::NewTcpConnection, ConnectionId, Port, RequestId};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{self, error::SendError, Receiver, Sender},
    task::JoinSet,
};

use self::{filtered::DynamicBody, unfiltered::UnfilteredStealTask};
use super::{http::DefaultReversibleStream, subscriptions::PortSubscription};
use crate::{http::HttpVersion, steal::connections::filtered::FilteredStealTask, util::ClientId};

mod filtered;
mod unfiltered;

/// Messages consumed by per-connection tasks.
pub enum ConnectionMessageIn {
    /// Client sent some bytes.
    Raw { client_id: ClientId, data: Vec<u8> },
    /// Client sent an HTTP response.
    Response {
        client_id: ClientId,
        request_id: RequestId,
        response: Response<DynamicBody>,
    },
    /// Client failed to send an HTTP response.
    ResponseFailed {
        client_id: ClientId,
        request_id: RequestId,
    },
    /// Client unsubscribed the connection.
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
    pub fn client_id(&self) -> ClientId {
        match self {
            Self::Raw { client_id, .. } => *client_id,
            Self::Response { client_id, .. } => *client_id,
            Self::ResponseFailed { client_id, .. } => *client_id,
            Self::Unsubscribed { client_id } => *client_id,
        }
    }
}

/// Update from a [`StolenConnection`] served by [`StolenConnections`].
pub enum ConnectionMessageOut {
    /// Received some bytes.
    Raw {
        client_id: ClientId,
        connection_id: ConnectionId,
        data: Vec<u8>,
    },
    /// Matched an incoming HTTP request with a client.
    Request {
        client_id: ClientId,
        connection_id: ConnectionId,
        request: Request<Incoming>,
        id: RequestId,
        port: Port,
    },
    /// Subscribed the client to a new TCP connection.
    SubscribedTcp {
        client_id: ClientId,
        connection: NewTcpConnection,
    },
    /// Subscribed the client to a new filtered HTTP connection.
    SubscribedHttp {
        client_id: ClientId,
        connection_id: ConnectionId,
    },
    /// Closed the connection.
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
                debug_struct.field("type", &"Opened");
                debug_struct.field("connection_id", &connection.connection_id);
                debug_struct.field("client_id", client_id);
                debug_struct.field("connection", connection);
            }
            Self::SubscribedHttp {
                client_id,
                connection_id,
            } => {
                debug_struct.field("type", &"Opened");
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

pub struct StolenConnections {
    next_connection_id: u64,

    connection_txs: HashMap<ConnectionId, Sender<ConnectionMessageIn>>,
    tasks: JoinSet<ConnectionId>,

    main_tx: Sender<ConnectionMessageOut>,
    main_rx: Receiver<ConnectionMessageOut>,
}

impl StolenConnections {
    pub fn with_capacity(capacity: usize) -> Self {
        let (main_tx, main_rx) = mpsc::channel(128);

        Self {
            next_connection_id: 0,

            connection_txs: HashMap::with_capacity(capacity),
            tasks: Default::default(),

            main_tx,
            main_rx,
        }
    }

    pub fn serve(&mut self, connection: StolenConnection) {
        let connection_id = self.next_connection_id;
        self.next_connection_id += 1;

        let (task_tx, task_rx) = mpsc::channel(16);
        let main_tx = self.main_tx.clone();

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

/// A stolen incoming TCP connection.
pub struct StolenConnection {
    /// Raw OS stream.
    pub stream: TcpStream,
    /// Source of this connection.
    pub source: SocketAddr,
    /// Destination of this connection.
    pub destination: SocketAddr,
    /// Subscription that triggered the steal.
    pub port_subscription: PortSubscription,
}

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

struct ConnectionTask {
    connection_id: ConnectionId,
    connection: StolenConnection,
    rx: Receiver<ConnectionMessageIn>,
    tx: Sender<ConnectionMessageOut>,
}

impl ConnectionTask {
    /// Timeout for detemining if the TCP stream is an incoming HTTP connection.
    /// Used to pick between [`UnfilteredStealTask`] and [`FilteredStealTask`] when the accepting
    /// port has [`PortSubscription::Filtered].
    const HTTP_DETECTION_TIMEOUT: Duration = Duration::from_secs(10);

    async fn run(self) -> Result<(), ConnectionTaskError> {
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
                    tx: self.tx,
                    rx: self.rx,
                };

                task.run().await
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
                    self.rx,
                    self.tx,
                );

                task.run().await
            }
        }
    }
}
