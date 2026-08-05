use std::{
    collections::{HashMap, HashSet, VecDeque},
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
use mirrord_quic::BiStream;
use socket_stream::SocketStream;
use streammap_ext::StreamMap;
use tokio::{
    io::{self, AsyncWriteExt, ReadHalf, WriteHalf},
    select,
    sync::{
        OwnedSemaphorePermit, Semaphore,
        mpsc::{self, Receiver, Sender, error::SendError},
    },
    task::JoinSet,
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::Level;

use crate::{
    data_stream::IncomingDataStream,
    error::AgentResult,
    metrics::TCP_OUTGOING_CONNECTION,
    outgoing::throttle::ThrottledStream,
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
};

pub(crate) mod seqpacket;
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
    ///
    /// * `fs_pid` - In targeted mode (both in pod and ephemeral), the PID of the main process. This
    ///   will be passed to an
    ///   [`InTargetPathResolver`](crate::util::path_resolver::InTargetPathResolver) to resolve unix
    ///   socket paths.
    ///
    /// * `data_streams` - yields the QUIC stream opened for each connection this task reports to
    ///   the client, and makes connections be spliced onto those streams rather than relayed as
    ///   mirrord-protocol messages. Pass [`None`] when the client is not on a QUIC connection that
    ///   negotiated [`DATA_STREAM_VERSION`](mirrord_quic::DATA_STREAM_VERSION).
    pub(crate) fn new(
        runtime: &BgTaskRuntime,
        pid: Option<u64>,
        data_streams: Option<Receiver<IncomingDataStream>>,
    ) -> Self {
        // IMPORTANT: this makes tokio tasks spawn on `runtime`.
        // Do not remove this.
        let _rt = runtime.handle().enter();

        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task_status =
            tokio::spawn(TcpOutgoingTask::new(pid, layer_rx, daemon_tx, data_streams).run())
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
    /// Data streams opened by the operator, one per connection. [`None`] when the client cannot
    /// carry connections on their own streams, in which case their bytes are relayed as
    /// mirrord-protocol messages through [`Self::writers`] and [`Self::readers`] instead.
    data_streams: Option<Receiver<IncomingDataStream>>,
    /// Connections that have been reported to the client and are waiting for their data stream.
    ///
    /// Nothing is read from these sockets yet, so anything the peer sends in the meantime stays in
    /// the kernel's receive buffer and is subject to normal TCP backpressure.
    awaiting_data_stream: HashMap<ConnectionId, SocketStream>,
    /// Connections currently being copied between their socket and their data stream.
    splices: JoinSet<ConnectionId>,
}

impl Drop for TcpOutgoingTask {
    fn drop(&mut self) {
        // A relayed connection is tracked once per io half, so its halves have to be counted as one
        // connection to match the single increment made when it was opened. Spliced connections and
        // ones still waiting for their stream are tracked once each.
        let relayed = self
            .readers
            .keys()
            .chain(self.writers.keys())
            .collect::<HashSet<_>>()
            .len();
        let connections = relayed + self.awaiting_data_stream.len() + self.splices.len();

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
    const THROTTLE_PERMITS: usize = Self::READ_BUFFER_SIZE * 8;

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
        data_streams: Option<Receiver<IncomingDataStream>>,
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
            data_streams,
            awaiting_data_stream: Default::default(),
            splices: JoinSet::new(),
        }
    }

    /// Whether connections should be spliced onto their own data stream rather than relayed as
    /// mirrord-protocol messages.
    fn splices_connections(&self) -> bool {
        self.data_streams.is_some()
    }

    /// Copies between an intercepted connection and the data stream carrying it, until both
    /// directions are done.
    ///
    /// Finishing the sending half of the stream is how the peer's `shutdown` is passed on, and
    /// dropping the stream without finishing it resets it, which is how the operator learns the
    /// connection failed. [`tokio::io::copy_bidirectional`] already propagates shutdown each way,
    /// so both fall out of its normal and error returns.
    async fn splice(
        connection_id: ConnectionId,
        mut socket: SocketStream,
        mut stream: BiStream,
    ) -> ConnectionId {
        match tokio::io::copy_bidirectional(&mut socket, &mut stream).await {
            Ok((from_stream, from_socket)) => {
                tracing::trace!(
                    connection_id,
                    from_stream,
                    from_socket,
                    "Spliced connection finished",
                );
            }
            Err(error) => {
                tracing::trace!(
                    connection_id,
                    %error,
                    "Spliced connection failed, resetting its data stream",
                );
            }
        }

        connection_id
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

                Some(data_stream) = Self::next_data_stream(&mut self.data_streams) => {
                    self.handle_data_stream(data_stream);
                    false
                }

                Some(finished) = self.splices.join_next() => {
                    if let Ok(connection_id) = finished {
                        TCP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::trace!(connection_id, "Spliced connection closed");
                    }
                    false
                }
            };

            if channel_closed {
                tracing::trace!("Client channel closed, exiting");
                break;
            }
        }
    }

    /// Waits for the next data stream, or never resolves when connections are not spliced.
    async fn next_data_stream(
        data_streams: &mut Option<Receiver<IncomingDataStream>>,
    ) -> Option<IncomingDataStream> {
        match data_streams {
            Some(receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Marries a data stream with the connection it carries and starts copying between them.
    #[tracing::instrument(level = Level::TRACE, skip(self, data_stream))]
    fn handle_data_stream(&mut self, data_stream: IncomingDataStream) {
        let IncomingDataStream {
            connection_id,
            stream,
        } = data_stream;

        let Some(socket) = self.awaiting_data_stream.remove(&connection_id) else {
            // The client closed the connection between us reporting it and the stream arriving.
            // Dropping the stream resets it, which is what we want to tell the operator anyway.
            tracing::trace!(
                connection_id,
                "Received a data stream for an unknown connection, dropping it",
            );
            return;
        };

        self.splices
            .spawn(Self::splice(connection_id, socket, stream));
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

            if self.splices_connections() {
                // The operator opens the data stream once it sees the message we are about to send,
                // so the socket waits here until then. Not reading from it yet is deliberate: it
                // leaves early data in the kernel's receive buffer under normal TCP backpressure,
                // instead of buffering it in the agent.
                self.awaiting_data_stream
                    .insert(connection_id, connected.stream);
            } else {
                let (read_half, write_half) = io::split(connected.stream);
                self.writers.insert(connection_id, write_half);
                self.readers.insert(
                    connection_id,
                    ThrottledStream::new(
                        ReaderStream::with_capacity(read_half, Self::READ_BUFFER_SIZE),
                        self.throttler.clone(),
                    ),
                );
            }
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
                if self.splices_connections() {
                    // Connections are carried on their own data streams, so their bytes never
                    // arrive as messages. An operator sending them anyway is out of step with the
                    // negotiated transport version, and dropping them beats tearing down a
                    // connection that is working.
                    tracing::trace!(
                        connection_id,
                        "Ignoring a write message for a spliced connection",
                    );
                    return Ok(());
                }

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
            //
            // A spliced connection is closed by resetting its data stream instead, so the only one
            // that can be closed this way is one still waiting for its stream. The metric is only
            // adjusted for a connection we were actually still tracking, so that a close for an
            // already-forgotten connection cannot drive the count below the truth.
            LayerTcpOutgoing::Close(LayerClose { connection_id }) => {
                let was_tracked = self.awaiting_data_stream.remove(&connection_id).is_some()
                    | self.writers.remove(&connection_id).is_some()
                    | self.readers.remove(&connection_id).is_some();

                if was_tracked {
                    TCP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }

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
