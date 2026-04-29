use std::{
    collections::{HashMap, VecDeque},
    fmt,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{FutureExt, Stream, future::BoxFuture, stream::FuturesUnordered};
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, RemoteError, RemoteResult, ResponseError,
    outgoing::{tcp::*, *},
    uid::Uid,
};
use socket_stream::SocketStream;
use streammap_ext::StreamMap;
use tokio::{
    io::{self, AsyncWriteExt, ReadHalf, WriteHalf},
    select,
    sync::{
        OwnedSemaphorePermit, Semaphore,
        mpsc::{self, Receiver, Sender, error::SendError},
    },
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::Level;

use crate::{
    error::AgentResult,
    metrics::TCP_OUTGOING_CONNECTION,
    outgoing::throttle::ThrottledStream,
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
};

mod socket_stream;
mod throttle;
mod udp;

pub(crate) use udp::UdpOutgoingApi;

/// Possibly throttled message.
pub(crate) struct Throttled<M> {
    pub(crate) message: M,
    /// This should be dropped **only** after sending [`Self::message`] to the client.
    pub(crate) throttle: Option<OwnedSemaphorePermit>,
}

impl<M> From<M> for Throttled<M> {
    fn from(message: M) -> Self {
        Self {
            message,
            throttle: None,
        }
    }
}

/// An interface for a background task handling [`LayerTcpOutgoing`] messages.
/// Each agent client has their own independent instance (neither this wrapper nor the background
/// task are shared).
pub(crate) struct TcpOutgoingApi {
    task_status: BgTaskStatus,

    /// Sends the layer messages to the [`TcpOutgoingTask`].
    layer_tx: Sender<LayerTcpOutgoing>,

    /// Reads the daemon messages from the [`TcpOutgoingTask`].
    daemon_rx: Receiver<Throttled<DaemonMessage>>,
}

impl TcpOutgoingApi {
    /// Spawns a new background task for handling the `outgoing` feature and creates a new instance
    /// of this struct to serve as an interface.
    ///
    /// # Params
    ///
    /// * `runtime` - tokio runtime to spawn the background task on.
    pub(crate) fn new(runtime: &BgTaskRuntime) -> Self {
        // IMPORTANT: this makes tokio tasks spawn on `runtime`.
        // Do not remove this.
        let _rt = runtime.handle().enter();

        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let pid = runtime.target_pid();
        let task_status = tokio::spawn(TcpOutgoingTask::new(pid, layer_rx, daemon_tx).run())
            .into_status("TcpOutgoingTask");

        Self {
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    /// Sends the [`LayerTcpOutgoing`] message to the background task.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn send_to_task(&mut self, message: LayerTcpOutgoing) -> AgentResult<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }

    /// Receives a [`DaemonTcpOutgoing`] message from the background task.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn recv_from_task(&mut self) -> AgentResult<Throttled<DaemonMessage>> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.wait_assert_running().await),
        }
    }
}

/// Handles outgoing connections for one client (layer).
struct TcpOutgoingTask {
    next_connection_id: ConnectionId,
    /// Writing halves of peer connections made on layer's requests.
    writers: HashMap<ConnectionId, WriteHalf<SocketStream>>,
    /// Reading halves of peer connections made on layer's requests.
    readers: StreamMap<ConnectionId, TcpReadStream>,
    /// Optional pid of agent's target. Used in [`SocketStream::connect`].
    pid: Option<u64>,
    layer_rx: Receiver<LayerTcpOutgoing>,
    daemon_tx: Sender<Throttled<DaemonMessage>>,
    connects_v1: FuturesQueue<BoxFuture<'static, RemoteResult<Connected>>>,
    connects_v2: FuturesUnordered<BoxFuture<'static, (RemoteResult<Connected>, Uid)>>,
    throttler: Arc<Semaphore>,
}

impl Drop for TcpOutgoingTask {
    fn drop(&mut self) {
        let connections = self.readers.keys().chain(self.writers.keys()).count();
        TCP_OUTGOING_CONNECTION.fetch_sub(connections, std::sync::atomic::Ordering::Relaxed);
    }
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
    /// How much incoming data we can accumulate in memory, before it's flushed to the client.
    ///
    /// This **must** be larger than [`Self::READ_BUFFER_SIZE`].
    const THROTTLE_PERMITS: usize = 512 * 1024;

    /// Timeout for connect attempts.
    ///
    /// # TODO(alex)
    /// This timeout works around the issue where golang tries to connect
    /// to an invalid socket address and hangs until the socket times out.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

    fn new(
        pid: Option<u64>,
        layer_rx: Receiver<LayerTcpOutgoing>,
        daemon_tx: Sender<Throttled<DaemonMessage>>,
    ) -> Self {
        Self {
            next_connection_id: 0,
            writers: Default::default(),
            readers: Default::default(),
            pid,
            layer_rx,
            daemon_tx,
            connects_v1: Default::default(),
            connects_v2: Default::default(),
            throttler: Arc::new(Semaphore::new(Self::THROTTLE_PERMITS)),
        }
    }

    /// Runs this task as long as the channels connecting it with the [`TcpOutgoingApi`] are open.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    async fn run(mut self) {
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

                Some(result) = self.connects_v1.next() => {
                    self.handle_connect_result(None, result).await.is_err()
                }

                Some((result, uid)) = self.connects_v2.next() => {
                    self.handle_connect_result(Some(uid), result).await.is_err()
                }
            };

            if channel_closed {
                tracing::trace!("Client channel closed, exiting");
                break;
            }
        }
    }

    /// Returns [`Err`] only when the client has disconnected.
    #[tracing::instrument(
        level = Level::TRACE,
        skip(read),
        fields(read = ?read.as_ref().map(|data| data.as_ref().map(|data| data.0.len()).unwrap_or_default()))
        err(level = Level::TRACE)
    )]
    async fn handle_connection_read(
        &mut self,
        connection_id: ConnectionId,
        read: io::Result<Option<(Bytes, OwnedSemaphorePermit)>>,
    ) -> Result<(), SendError<Throttled<DaemonMessage>>> {
        match read {
            // New bytes came in from a peer connection.
            // We pass them to the layer.
            Ok(Some((read, permits))) => {
                let message = DaemonTcpOutgoing::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: read.into(),
                }));
                self.daemon_tx
                    .send(Throttled {
                        message: DaemonMessage::TcpOutgoing(message),
                        throttle: Some(permits),
                    })
                    .await?;
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
                TCP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                self.daemon_tx
                    .send(
                        DaemonMessage::LogMessage(LogMessage::warn(format!(
                            "read from outgoing connection {connection_id} failed: {error}"
                        )))
                        .into(),
                    )
                    .await?;
                self.daemon_tx
                    .send(
                        DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(connection_id)).into(),
                    )
                    .await?;
            }

            // EOF occurred in one of peer connections.
            // We send 0-sized read to the layer to inform about the shutdown condition.
            // Reader removal is handled internally by the `StreamMap`.
            Ok(None) => {
                tracing::trace!(
                    connection_id,
                    "Peer connection shutdown, sending 0-sized read message.",
                );

                let message = DaemonTcpOutgoing::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: vec![].into(),
                }));
                self.daemon_tx
                    .send(DaemonMessage::TcpOutgoing(message).into())
                    .await?;

                // If the writing half is not found, it means that the layer has already shut down
                // its side of the connection. We send a closing message to clean
                // everything up.
                if !self.writers.contains_key(&connection_id) {
                    tracing::trace!(
                        connection_id,
                        "Layer connection is shut down as well, sending close message.",
                    );

                    TCP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                    self.daemon_tx
                        .send(
                            DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(connection_id))
                                .into(),
                        )
                        .await?;
                }
            }
        }

        Ok(())
    }

    async fn connect(
        remote_address: SocketAddress,
        target_pid: Option<u64>,
    ) -> RemoteResult<Connected> {
        let started_at = Instant::now();
        let socket_stream = tokio::time::timeout(
            Self::CONNECT_TIMEOUT,
            SocketStream::connect(remote_address.clone(), target_pid),
        )
        .await
        .map_err(|_| {
            ResponseError::Remote(RemoteError::ConnectTimedOut(remote_address.clone()))
        })??;
        tracing::debug!(
            %remote_address,
            elapsed = ?started_at.elapsed(),
            "Outgoing connection made",
        );
        let local_address = socket_stream.local_addr()?;
        Ok(Connected {
            stream: socket_stream,
            remote_address,
            local_address,
        })
    }

    async fn handle_connect_result(
        &mut self,
        uid: Option<Uid>,
        result: RemoteResult<Connected>,
    ) -> Result<(), SendError<Throttled<DaemonMessage>>> {
        let message = result.map(|connected| {
            let connection_id = self.next_connection_id;
            self.next_connection_id += 1;

            let (read_half, write_half) = io::split(connected.stream);
            self.writers.insert(connection_id, write_half);
            self.readers.insert(
                connection_id,
                ThrottledStream::new(
                    ReaderStream::with_capacity(read_half, Self::READ_BUFFER_SIZE),
                    self.throttler.clone(),
                ),
            );
            TCP_OUTGOING_CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            DaemonConnect {
                connection_id,
                remote_address: connected.remote_address,
                local_address: connected.local_address,
            }
        });

        let message = match uid {
            Some(uid) => DaemonTcpOutgoing::ConnectV2(DaemonConnectV2 {
                uid,
                connect: message,
            }),
            None => DaemonTcpOutgoing::Connect(message),
        };

        self.daemon_tx
            .send(DaemonMessage::TcpOutgoing(message).into())
            .await
    }

    /// Returns [`Err`] only when the client has disconnected.
    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_layer_msg(
        &mut self,
        message: LayerTcpOutgoing,
    ) -> Result<(), SendError<Throttled<DaemonMessage>>> {
        match message {
            // We make connection to the requested address, split the stream into halves with
            // `io::split`, and put them into respective maps.
            LayerTcpOutgoing::Connect(LayerConnect { remote_address }) => {
                let fut = Self::connect(remote_address, self.pid).boxed();
                self.connects_v1.push(fut);
                Ok(())
            }

            LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
                uid,
                remote_address,
            }) => {
                let fut = Self::connect(remote_address, self.pid)
                    .map(move |result| (result, uid))
                    .boxed();
                self.connects_v2.push(fut);
                Ok(())
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
                            TCP_OUTGOING_CONNECTION
                                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                            self.daemon_tx
                                .send(
                                    DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(
                                        connection_id,
                                    ))
                                    .into(),
                                )
                                .await?;

                            Ok(())
                        }
                    }

                    Ok(()) => Ok(()),

                    Err(error) => {
                        self.writers.remove(&connection_id);
                        self.readers.remove(&connection_id);
                        TCP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                        tracing::trace!(
                            connection_id,
                            ?error,
                            "Failed to handle layer write, sending close message to the client.",
                        );
                        self.daemon_tx
                            .send(
                                DaemonMessage::LogMessage(LogMessage::warn(format!(
                                    "write to outgoing connection {connection_id} failed: {error}"
                                )))
                                .into(),
                            )
                            .await?;
                        self.daemon_tx
                            .send(
                                DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(connection_id))
                                    .into(),
                            )
                            .await?;

                        Ok(())
                    }
                }
            }

            // Layer closed a connection entirely.
            // We remove io halves and forget about it.
            LayerTcpOutgoing::Close(LayerClose { connection_id }) => {
                self.writers.remove(&connection_id);
                self.readers.remove(&connection_id);
                TCP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                Ok(())
            }
        }
    }
}

type TcpReadStream = ThrottledStream<ReaderStream<ReadHalf<SocketStream>>>;

/// Established outgoing connection.
struct Connected {
    stream: SocketStream,
    remote_address: SocketAddress,
    local_address: SocketAddress,
}

/// FIFO queue of futures, implements [`Stream`].
///
/// The futures **not** polled in parallel.
/// Only the oldest future is polled.
struct FuturesQueue<F> {
    inner: VecDeque<F>,
}

impl<F> FuturesQueue<F> {
    fn push(&mut self, fut: F) {
        self.inner.push_back(fut);
    }
}

impl<F> Default for FuturesQueue<F> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl<F: Future + Unpin> Stream for FuturesQueue<F> {
    type Item = F::Output;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(fut) = this.inner.front_mut() else {
            return Poll::Ready(None);
        };

        let result = std::task::ready!(Pin::new(fut).poll(cx));

        this.inner.pop_front();
        if this.inner.len() < this.inner.capacity() / 3 {
            this.inner.shrink_to_fit();
        }

        Poll::Ready(Some(result))
    }
}
