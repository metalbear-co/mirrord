use std::{collections::HashMap, fmt, thread, time::Duration};

use bytes::Bytes;
use mirrord_protocol::{
    outgoing::{tcp::*, *},
    ConnectionId, RemoteError, ResponseError,
};
use socket_stream::SocketStream;
use streammap_ext::StreamMap;
use tokio::{
    io::{self, AsyncWriteExt, ReadHalf, WriteHalf},
    select,
    sync::mpsc::{self, error::SendError, Receiver, Sender},
    time,
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::Level;

use crate::{
    error::Result,
    util::run_thread_in_namespace,
    watched_task::{TaskStatus, WatchedTask},
};

mod socket_stream;
mod udp;

pub(crate) use udp::UdpOutgoingApi;

/// An interface for a background task handling [`LayerTcpOutgoing`] messages.
/// Each agent client has their own independent instance (neither this wrapper nor the background
/// task are shared).
pub(crate) struct TcpOutgoingApi {
    /// Holds the thread in which [`TcpOutgoingTask`] is running.
    _task: thread::JoinHandle<()>,

    /// Status of the [`TcpOutgoingTask`].
    task_status: TaskStatus,

    /// Sends the layer messages to the [`TcpOutgoingTask`].
    layer_tx: Sender<LayerTcpOutgoing>,

    /// Reads the daemon messages from the [`TcpOutgoingTask`].
    daemon_rx: Receiver<DaemonTcpOutgoing>,
}

impl TcpOutgoingApi {
    const TASK_NAME: &'static str = "TcpOutgoing";

    /// Spawns a new background task for handling `outgoing` feature and creates a new instance of
    /// this struct to serve as an interface.
    ///
    /// # Params
    ///
    /// * `pid` - process id of the agent's target container
    #[tracing::instrument(level = Level::TRACE)]
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let watched_task = WatchedTask::new(
            Self::TASK_NAME,
            TcpOutgoingTask::new(pid, layer_rx, daemon_tx).run(),
        );
        let task_status = watched_task.status();
        let task = run_thread_in_namespace(
            watched_task.start(),
            Self::TASK_NAME.to_string(),
            pid,
            "net",
        );

        Self {
            _task: task,
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    /// Sends the [`LayerTcpOutgoing`] message to the background task.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn send_to_task(&mut self, message: LayerTcpOutgoing) -> Result<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Receives a [`DaemonTcpOutgoing`] message from the background task.
    pub(crate) async fn recv_from_task(&mut self) -> Result<DaemonTcpOutgoing> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.unwrap_err().await),
        }
    }
}

/// Handles outgoing connections for one client (layer).
struct TcpOutgoingTask {
    next_connection_id: ConnectionId,
    /// Writing halves of peer connections made on layer's requests.
    writers: HashMap<ConnectionId, WriteHalf<SocketStream>>,
    /// Reading halves of peer connections made on layer's requests.
    readers: StreamMap<ConnectionId, ReaderStream<ReadHalf<SocketStream>>>,
    /// Optional pid of agent's target. Used in [`SocketStream::connect`].
    pid: Option<u64>,
    layer_rx: Receiver<LayerTcpOutgoing>,
    daemon_tx: Sender<DaemonTcpOutgoing>,
}

impl fmt::Debug for TcpOutgoingTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpOutgoingTask")
            .field("next_connection_id", &self.next_connection_id)
            .field("writers", &self.writers.len())
            .field("readers", &self.readers.len())
            .field("pid", &self.pid)
            .finish()
    }
}

impl TcpOutgoingTask {
    /// Buffer size for reading from the outgoing connections.
    const READ_BUFFER_SIZE: usize = 64 * 1024;

    /// Timeout for connect attempts.
    ///
    /// # TODO(alex)
    /// This timeout works around the issue where golang tries to connect
    /// to an invalid socket address and hangs until the socket times out.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

    fn new(
        pid: Option<u64>,
        layer_rx: Receiver<LayerTcpOutgoing>,
        daemon_tx: Sender<DaemonTcpOutgoing>,
    ) -> Self {
        Self {
            next_connection_id: 0,
            writers: Default::default(),
            readers: Default::default(),
            pid,
            layer_rx,
            daemon_tx,
        }
    }

    /// Runs this task as long as the channels connecting it with [`TcpOutgoingApi`] are open.
    /// This routine never fails and returns [`Result`] only due to [`WatchedTask`] constraints.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    async fn run(mut self) -> Result<()> {
        loop {
            let channel_closed = select! {
                biased;

                message = self.layer_rx.recv() => match message {
                    // We have a message from the layer to be handled.
                    Some(message) => {
                        self.handle_layer_msg(message).await.is_err()
                    },
                    // Our channel with the layer is closed, this task is no longer needed.
                    None => true,
                },

                // We have data coming from one of our peers.
                Some((connection_id, remote_read)) = self.readers.next() => {
                    self.handle_connection_read(connection_id, remote_read.transpose()).await.is_err()
                },
            };

            if channel_closed {
                tracing::trace!("Client channel closed, exiting");
                break Ok(());
            }
        }
    }

    /// Returns [`Err`] only when the client has disconnected.
    #[tracing::instrument(
        level = Level::TRACE,
        skip(read),
        fields(read = ?read.as_ref().map(|data| data.as_ref().map(Bytes::len).unwrap_or_default()))
        err(level = Level::TRACE)
    )]
    async fn handle_connection_read(
        &mut self,
        connection_id: ConnectionId,
        read: io::Result<Option<Bytes>>,
    ) -> Result<(), SendError<DaemonTcpOutgoing>> {
        match read {
            // New bytes came in from a peer connection.
            // We pass them to the layer.
            Ok(Some(read)) => {
                let message = DaemonTcpOutgoing::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: read.to_vec(),
                }));

                self.daemon_tx.send(message).await?;
            }

            // An error occurred when reading from a peer connection.
            // We remove both io halves and inform the layer that the connection is closed.
            // We remove the reader, because otherwise the `StreamMap` will produce an extra `None`
            // item from the related stream.
            Err(error) => {
                tracing::trace!(
                    ?error,
                    connection_id,
                    "Reading from peer connection failed, sending close message.",
                );

                self.readers.remove(&connection_id);
                self.writers.remove(&connection_id);

                let daemon_message = DaemonTcpOutgoing::Close(connection_id);
                self.daemon_tx.send(daemon_message).await?;
            }

            // EOF occurred in one of peer connections.
            // We send 0-sized read to the layer to inform about the shutdown condition.
            // Reader removal is handled internally by the `StreamMap`.
            Ok(None) => {
                tracing::trace!(
                    connection_id,
                    "Peer connection shutdown, sending 0-sized read message.",
                );

                let daemon_message = DaemonTcpOutgoing::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: vec![],
                }));

                self.daemon_tx.send(daemon_message).await?;

                // If the writing half is not found, it means that the layer has already shut down
                // its side of the connection. We send a closing message to clean
                // everything up.
                if !self.writers.contains_key(&connection_id) {
                    tracing::trace!(
                        connection_id,
                        "Layer connection is shut down as well, sending close message.",
                    );

                    self.daemon_tx
                        .send(DaemonTcpOutgoing::Close(connection_id))
                        .await?;
                }
            }
        }

        Ok(())
    }

    /// Returns [`Err`] only when the client has disconnected.
    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_layer_msg(
        &mut self,
        message: LayerTcpOutgoing,
    ) -> Result<(), SendError<DaemonTcpOutgoing>> {
        match message {
            // We make connection to the requested address, split the stream into halves with
            // `io::split`, and put them into respective maps.
            LayerTcpOutgoing::Connect(LayerConnect { remote_address }) => {
                let daemon_connect = time::timeout(
                    Self::CONNECT_TIMEOUT,
                    SocketStream::connect(remote_address.clone(), self.pid),
                )
                .await
                .unwrap_or_else(|_elapsed| {
                    Err(ResponseError::Remote(RemoteError::ConnectTimedOut(
                        remote_address.clone(),
                    )))
                })
                .and_then(|remote_stream| {
                    let agent_address = remote_stream.local_addr()?;
                    let connection_id = self.next_connection_id;
                    self.next_connection_id += 1;

                    let (read_half, write_half) = io::split(remote_stream);
                    self.writers.insert(connection_id, write_half);
                    self.readers.insert(
                        connection_id,
                        ReaderStream::with_capacity(read_half, Self::READ_BUFFER_SIZE),
                    );

                    Ok(DaemonConnect {
                        connection_id,
                        remote_address,
                        local_address: agent_address,
                    })
                });

                tracing::trace!(
                    result = ?daemon_connect,
                    "Connection attempt finished.",
                );
                self.daemon_tx
                    .send(DaemonTcpOutgoing::Connect(daemon_connect))
                    .await
            }

            // This message handles two cases:
            // 1. 0-sized writes mean shutdown condition on the layer side. We call shutdown on this
            //    connection's writer and remove it. If we don't find the reader, it means that the
            //    peer has already shut down the connection. In this case we send a closing message
            //    to the layer.
            // 2. all other writes mean that the layer sent some data through the connection. We
            //    pass it to this connection's writer.
            LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
            }) => {
                let write_result = match self.writers.get_mut(&connection_id) {
                    Some(writer) if bytes.is_empty() => {
                        tracing::trace!(
                            connection_id,
                            "Received 0-sized write from layer, shutting down peer connection."
                        );

                        writer.shutdown().await.map_err(ResponseError::from)
                    }

                    Some(writer) => writer.write_all(&bytes).await.map_err(ResponseError::from),

                    None => Err(ResponseError::NotFound(connection_id)),
                };

                match write_result {
                    Ok(()) if bytes.is_empty() => {
                        self.writers.remove(&connection_id);

                        if self.readers.contains_key(&connection_id) {
                            Ok(())
                        } else {
                            tracing::trace!(
                                connection_id,
                                "Peer connection is shut down as well, sending close message to the client.",
                            );
                            self.daemon_tx
                                .send(DaemonTcpOutgoing::Close(connection_id))
                                .await
                        }
                    }

                    Ok(()) => Ok(()),

                    Err(error) => {
                        self.writers.remove(&connection_id);
                        self.readers.remove(&connection_id);

                        tracing::trace!(
                            connection_id,
                            ?error,
                            "Failed to handle layer write, sending close message to the client.",
                        );
                        self.daemon_tx
                            .send(DaemonTcpOutgoing::Close(connection_id))
                            .await
                    }
                }
            }

            // Layer closed a connection entirely.
            // We remove io halves and forget about it.
            LayerTcpOutgoing::Close(LayerClose { connection_id }) => {
                self.writers.remove(&connection_id);
                self.readers.remove(&connection_id);

                Ok(())
            }
        }
    }
}
