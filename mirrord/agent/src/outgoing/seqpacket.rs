use std::{
    collections::HashMap,
    ffi::OsString,
    fmt,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use futures::{FutureExt, Stream, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, RemoteError, RemoteResult, ResponseError,
    outgoing::{UnixAddr, tcp::*, *},
    uid::Uid,
};
use streammap_ext::StreamMap;
use tokio::{
    io, select,
    sync::{
        OwnedSemaphorePermit, Semaphore,
        mpsc::{self, Receiver, Sender, error::SendError},
    },
};
use tokio_seqpacket::UnixSeqpacket;
use tracing::Level;

use crate::{
    error::AgentResult,
    metrics::SEQPACKET_CONNECTION,
    outgoing::{Throttled, throttle::ThrottledStream},
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
    util::path_resolver::InTargetPathResolver,
};

pub(crate) struct SeqpacketApi {
    task_status: BgTaskStatus,

    layer_tx: Sender<LayerSeqpacket>,

    daemon_rx: Receiver<Throttled<DaemonMessage>>,
}

impl SeqpacketApi {
    pub(crate) fn new(runtime: &BgTaskRuntime, pid: Option<u64>) -> Self {
        // IMPORTANT: this makes tokio tasks spawn on `runtime`.
        // Do not remove this.
        let _rt = runtime.handle().enter();

        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task_status = tokio::spawn(SeqpacketTask::new(pid, layer_rx, daemon_tx).run())
            .into_status("SeqpacketTask");

        Self {
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn send_to_task(&mut self, message: LayerSeqpacket) -> AgentResult<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }

    /// Receives a [`DaemonSeqpacket`] message from the background task.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn recv_from_task(&mut self) -> AgentResult<Throttled<DaemonMessage>> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.wait_assert_running().await),
        }
    }
}

struct SeqpacketTask {
    next_connection_id: ConnectionId,
    /// Writing handles of peer connections made on layer's requests.
    writers: HashMap<ConnectionId, Arc<UnixSeqpacket>>,
    /// Reading handles of peer connections made on layer's requests.
    readers: StreamMap<ConnectionId, SeqpacketReadStream>,
    /// Optional pid of agent's target. Used for resolving pathname socket addresses.
    pid: Option<u64>,
    layer_rx: Receiver<LayerSeqpacket>,
    daemon_tx: Sender<Throttled<DaemonMessage>>,
    connects: FuturesUnordered<BoxFuture<'static, (RemoteResult<ConnectedSeqpacket>, Uid)>>,
    throttler: Arc<Semaphore>,
}

impl Drop for SeqpacketTask {
    fn drop(&mut self) {
        let connections = self.readers.keys().chain(self.writers.keys()).count();
        SEQPACKET_CONNECTION.fetch_sub(connections, std::sync::atomic::Ordering::Relaxed);
    }
}

impl fmt::Debug for SeqpacketTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeqpacketTask")
            .field("next_connection_id", &self.next_connection_id)
            .field("writers", &self.writers.len())
            .field("readers", &self.readers.len())
            .field("pid", &self.pid)
            .finish()
    }
}

impl SeqpacketTask {
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
        layer_rx: Receiver<LayerSeqpacket>,
        daemon_tx: Sender<Throttled<DaemonMessage>>,
    ) -> Self {
        Self {
            next_connection_id: 0,
            writers: Default::default(),
            readers: Default::default(),
            pid,
            layer_rx,
            daemon_tx,
            connects: Default::default(),
            throttler: Arc::new(Semaphore::new(Self::THROTTLE_PERMITS)),
        }
    }

    /// Runs this task as long as the channels connecting it with the [`SeqpacketApi`] are open.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    async fn run(mut self) {
        loop {
            let channel_closed = select! {
                biased;

                message = self.layer_rx.recv() => match message {
                    Some(message) => self.handle_layer_msg(message).await.is_err(),
                    None => true,
                },

                Some((connection_id, remote_read)) = self.readers.next() => {
                    self.handle_connection_read(connection_id, remote_read.transpose()).await.is_err()
                },

                Some((result, uid)) = self.connects.next() => {
                    self.handle_connect_result(uid, result).await.is_err()
                }
            };

            if channel_closed {
                tracing::trace!("Client channel closed, exiting");
                break;
            }
        }
    }

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
            Ok(Some((read, permits))) => {
                let message = DaemonSeqpacket::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: read.into(),
                }));
                self.daemon_tx
                    .send(Throttled {
                        message: DaemonMessage::SeqpacketOutgoing(message),
                        throttle: Some(permits),
                    })
                    .await?;
            }
            Err(error) => {
                tracing::trace!(
                    ?error,
                    connection_id,
                    "Reading from seqpacket connection failed, sending close message.",
                );

                self.readers.remove(&connection_id);
                self.writers.remove(&connection_id);
                SEQPACKET_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                self.daemon_tx
                    .send(
                        DaemonMessage::LogMessage(LogMessage::warn(format!(
                            "read from outgoing seqpacket connection {connection_id} failed: {error}"
                        )))
                        .into(),
                    )
                    .await?;
                self.daemon_tx
                    .send(
                        DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Close(connection_id))
                            .into(),
                    )
                    .await?;
            }
            Ok(None) => {
                tracing::trace!(connection_id, "Seqpacket peer connection closed.");

                self.readers.remove(&connection_id);
                self.writers.remove(&connection_id);
                SEQPACKET_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                self.daemon_tx
                    .send(
                        DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Close(connection_id))
                            .into(),
                    )
                    .await?;
            }
        }

        Ok(())
    }

    async fn connect(
        remote_address: SocketAddress,
        target_pid: Option<u64>,
    ) -> RemoteResult<ConnectedSeqpacket> {
        let started_at = Instant::now();
        let path = Self::connect_path(remote_address.clone(), target_pid)?;
        let socket =
            tokio::time::timeout(Self::CONNECT_TIMEOUT, UnixSeqpacket::connect(path)).await;
        let socket = socket.map_err(|_| {
            ResponseError::Remote(RemoteError::ConnectTimedOut(remote_address.clone()))
        })??;

        tracing::debug!(
            %remote_address,
            elapsed = ?started_at.elapsed(),
            "Outgoing seqpacket connection made",
        );

        Ok(ConnectedSeqpacket {
            socket,
            remote_address,
            local_address: SocketAddress::Unix(UnixAddr::Unnamed),
        })
    }

    fn connect_path(
        remote_address: SocketAddress,
        target_pid: Option<u64>,
    ) -> RemoteResult<PathBuf> {
        match remote_address {
            SocketAddress::Unix(UnixAddr::Pathname(path)) => {
                if let Some(pid) = target_pid {
                    Ok(InTargetPathResolver::new(pid).resolve(&path)?)
                } else {
                    Ok(path)
                }
            }
            SocketAddress::Unix(UnixAddr::Abstract(mut name)) => {
                name.insert(0, 0);
                Ok(OsString::from_vec(name).into())
            }
            address => Err(ResponseError::Remote(RemoteError::InvalidAddress(address))),
        }
    }

    async fn handle_connect_result(
        &mut self,
        uid: Uid,
        result: RemoteResult<ConnectedSeqpacket>,
    ) -> Result<(), SendError<Throttled<DaemonMessage>>> {
        let message = result.map(|connected| {
            let connection_id = self.next_connection_id;
            self.next_connection_id += 1;

            let socket = Arc::new(connected.socket);
            self.writers.insert(connection_id, socket.clone());
            self.readers.insert(
                connection_id,
                ThrottledStream::new(
                    SeqpacketReadHalf {
                        socket,
                        buffer: BytesMut::with_capacity(Self::READ_BUFFER_SIZE),
                    },
                    self.throttler.clone(),
                ),
            );
            SEQPACKET_CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            DaemonConnect {
                connection_id,
                remote_address: connected.remote_address,
                local_address: connected.local_address,
            }
        });

        let message = DaemonSeqpacket::ConnectV2(DaemonConnectV2 {
            uid,
            connect: message,
        });

        self.daemon_tx
            .send(DaemonMessage::SeqpacketOutgoing(message).into())
            .await
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_layer_msg(
        &mut self,
        message: LayerSeqpacket,
    ) -> Result<(), SendError<Throttled<DaemonMessage>>> {
        match message {
            LayerSeqpacket::ConnectV2(LayerConnectV2 {
                uid,
                remote_address,
            }) => {
                let fut = Self::connect(remote_address, self.pid)
                    .map(move |result| (result, uid))
                    .boxed();
                self.connects.push(fut);
                Ok(())
            }
            LayerSeqpacket::Write(LayerWrite {
                connection_id,
                bytes,
            }) => {
                let write_result = match self.writers.get(&connection_id) {
                    Some(socket) => socket
                        .send(&bytes)
                        .await
                        .map_err(ResponseError::from)
                        .and_then(|written| {
                            if written == bytes.len() {
                                Ok(())
                            } else {
                                Err(ResponseError::from(io::Error::new(
                                    io::ErrorKind::WriteZero,
                                    "partial seqpacket write",
                                )))
                            }
                        }),
                    None => Err(ResponseError::NotFound(connection_id)),
                };

                match write_result {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        self.writers.remove(&connection_id);
                        self.readers.remove(&connection_id);
                        SEQPACKET_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                        tracing::trace!(
                            connection_id,
                            ?error,
                            "Failed to handle seqpacket layer write, sending close message to the client.",
                        );
                        self.daemon_tx
                            .send(
                                DaemonMessage::LogMessage(LogMessage::warn(format!(
                                    "write to outgoing seqpacket connection {connection_id} failed: {error}"
                                )))
                                .into(),
                            )
                            .await?;
                        self.daemon_tx
                            .send(
                                DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Close(
                                    connection_id,
                                ))
                                .into(),
                            )
                            .await?;

                        Ok(())
                    }
                }
            }
            LayerSeqpacket::Close(LayerClose { connection_id }) => {
                self.writers.remove(&connection_id);
                self.readers.remove(&connection_id);
                SEQPACKET_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                Ok(())
            }
        }
    }
}

type SeqpacketReadStream = ThrottledStream<SeqpacketReadHalf>;

struct SeqpacketReadHalf {
    socket: Arc<UnixSeqpacket>,
    buffer: BytesMut,
}

impl Stream for SeqpacketReadHalf {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.buffer.resize(SeqpacketTask::READ_BUFFER_SIZE, 0);

        match this.socket.poll_recv(cx, &mut this.buffer) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(0)) => Poll::Ready(None),
            Poll::Ready(Ok(read)) => {
                this.buffer.truncate(read);
                Poll::Ready(Some(Ok(this.buffer.split().freeze())))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(error))),
        }
    }
}

struct ConnectedSeqpacket {
    socket: UnixSeqpacket,
    remote_address: SocketAddress,
    local_address: SocketAddress,
}
