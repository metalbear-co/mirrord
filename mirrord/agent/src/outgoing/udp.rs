use core::fmt;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use bytes::{Bytes, BytesMut};
use futures::{SinkExt, Stream, StreamExt};
use mirrord_protocol::{
    ConnectionId, RemoteResult, ResponseError,
    outgoing::{udp::*, *},
};
use streammap_ext::StreamMap;
use tokio::{
    io,
    net::UdpSocket,
    select,
    sync::{
        OwnedSemaphorePermit, Semaphore,
        mpsc::{self, Receiver, Sender, error::SendError},
    },
};
use tokio_util::{codec::BytesCodec, udp::UdpFramed};
use tracing::Level;

use crate::{
    error::AgentResult,
    metrics::UDP_OUTGOING_CONNECTION,
    outgoing::{Throttled, throttle::ThrottledStream},
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
};

/// Task that handles [`LayerUdpOutgoing`] and [`DaemonUdpOutgoing`] messages.
///
/// We start these tasks from the [`UdpOutgoingApi`] on a [`BgTaskRuntime`].
struct UdpOutgoingTask {
    next_connection_id: ConnectionId,
    /// Writing halves of peer connections made on layer's requests.
    #[allow(clippy::type_complexity)]
    writers: HashMap<ConnectionId, (UdpFramed<BytesCodec, Arc<UdpSocket>>, SocketAddr)>,
    /// Reading halves of peer connections made on layer's requests.
    readers: StreamMap<ConnectionId, UdpReadStream>,
    /// Optional pid of agent's target. Used in `SocketStream::connect`.
    pid: Option<u64>,
    layer_rx: Receiver<LayerUdpOutgoing>,
    daemon_tx: Sender<Throttled<DaemonUdpOutgoing>>,
    throttler: Arc<Semaphore>,
}

impl Drop for UdpOutgoingTask {
    fn drop(&mut self) {
        let connections = self.readers.keys().chain(self.writers.keys()).count();
        UDP_OUTGOING_CONNECTION.fetch_sub(connections, std::sync::atomic::Ordering::Relaxed);
    }
}

impl fmt::Debug for UdpOutgoingTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UdpOutgoingTask")
            .field("next_connection_id", &self.next_connection_id)
            .field("writers", &self.writers.len())
            .field("readers", &self.readers.len())
            .field("pid", &self.pid)
            .finish()
    }
}

impl UdpOutgoingTask {
    /// How much incoming data we can accumulate in memory, before it's flushed to the client.
    ///
    /// This **must** be larger than maximal size of a UDP packet (64kb).
    const THROTTLE_PERMITS: usize = 512 * 1024;

    fn new(
        pid: Option<u64>,
        layer_rx: Receiver<LayerUdpOutgoing>,
        daemon_tx: Sender<Throttled<DaemonUdpOutgoing>>,
    ) -> Self {
        Self {
            next_connection_id: 0,
            writers: Default::default(),
            readers: Default::default(),
            pid,
            layer_rx,
            daemon_tx,
            throttler: Arc::new(Semaphore::new(Self::THROTTLE_PERMITS)),
        }
    }

    /// Runs this task as long as the channels connecting it with the [`UdpOutgoingApi`] are open.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(super) async fn run(mut self) {
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
    ) -> Result<(), SendError<Throttled<DaemonUdpOutgoing>>> {
        match read {
            Ok(Some((read, permits))) => {
                let message = DaemonUdpOutgoing::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: read.into(),
                }));

                self.daemon_tx
                    .send(Throttled {
                        message,
                        throttle: Some(permits),
                    })
                    .await?
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
                UDP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                self.daemon_tx.send(daemon_message.into()).await?;
            }
            Ok(None) => {
                self.writers.remove(&connection_id);
                self.readers.remove(&connection_id);
                UDP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                self.daemon_tx.send(daemon_message.into()).await?;
            }
        }

        Ok(())
    }

    /// Connects to the remote address.
    ///
    /// This includes binding a [`UdpSocket`] and calling [`UdpSocket::connect`].
    /// Note that these operations do not require any IO, as [`SocketAddress`] cannot be a hostname.
    /// Therefore, this function is async only due to [`UdpSocket`] interface's constraints.
    ///
    /// Handles:
    /// 1. Normal `connect` called on an udp socket by the user, through the [`LayerConnect`]
    ///    message;
    /// 2. DNS special-case connection that comes on port `53`, where we have a hack that fakes a
    ///    connected udp socket. This case in particular requires that the user enable file ops with
    ///    read access to `/etc/resolv.conf`, otherwise they'll be getting a mismatched connection;
    /// 3. User is trying to use `sendto` and `recvfrom`, we use the same hack as in DNS to fake a
    ///    connection.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::DEBUG))]
    async fn connect(&mut self, remote_address: SocketAddress) -> RemoteResult<DaemonConnect> {
        let peer_addr = remote_address.clone().try_into()?;
        let bind_addr = match peer_addr {
            std::net::SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            std::net::SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };

        let socket = UdpSocket::bind(bind_addr).await?;
        socket.connect(peer_addr).await?;

        let connection_id = self.next_connection_id;
        self.next_connection_id += 1;

        let peer_address = socket.peer_addr()?;
        let local_address = socket.local_addr()?;
        let local_address = SocketAddress::Ip(local_address);

        let socket = Arc::new(socket);
        let writer = UdpFramed::new(socket.clone(), BytesCodec::new());
        let reader = ThrottledStream::new(
            UdpReadHalf {
                socket,
                buffer: BytesMut::with_capacity(64 * 1024),
            },
            self.throttler.clone(),
        );

        self.writers.insert(connection_id, (writer, peer_address));
        self.readers.insert(connection_id, reader);
        UDP_OUTGOING_CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        })
    }

    /// Returns [`Err`] only when the client has disconnected.
    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_layer_msg(
        &mut self,
        message: LayerUdpOutgoing,
    ) -> Result<(), SendError<Throttled<DaemonUdpOutgoing>>> {
        match message {
            // [user] -> [layer] -> [agent] -> [layer]
            // `user` is asking us to connect to some remote host.
            LayerUdpOutgoing::Connect(LayerConnect { remote_address }) => {
                let daemon_connect = self.connect(remote_address).await;
                tracing::trace!(
                    result = ?daemon_connect,
                    "Connection attempt finished.",
                );
                self.daemon_tx
                    .send(DaemonUdpOutgoing::Connect(daemon_connect).into())
                    .await?;
                Ok(())
            }
            // [user] -> [layer] -> [agent] -> [layer]
            // `user` is asking us to connect to some remote host.
            LayerUdpOutgoing::ConnectV2(LayerConnectV2 {
                uid,
                remote_address,
            }) => {
                let connect = self.connect(remote_address).await;
                let daemon_connect = DaemonConnectV2 { uid, connect };
                tracing::trace!(
                    result = ?daemon_connect,
                    "Connection attempt finished.",
                );
                self.daemon_tx
                    .send(DaemonUdpOutgoing::ConnectV2(daemon_connect).into())
                    .await?;
                Ok(())
            }
            // [user] -> [layer] -> [agent] -> [remote]
            // `user` wrote some message to the remote host.
            LayerUdpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
            }) => {
                let write_result = match self
                    .writers
                    .get_mut(&connection_id)
                    .ok_or(ResponseError::NotFound(connection_id))
                {
                    Ok((mirror, remote_address)) => mirror
                        .send((bytes.0, *remote_address))
                        .await
                        .map_err(ResponseError::from),
                    Err(fail) => Err(fail),
                };

                match write_result {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        self.writers.remove(&connection_id);
                        self.readers.remove(&connection_id);
                        UDP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                        tracing::trace!(
                            connection_id,
                            ?error,
                            "Failed to handle layer write, sending close message to the client.",
                        );

                        let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                        self.daemon_tx.send(daemon_message.into()).await?;

                        Ok(())
                    }
                }
            }
            // [layer] -> [agent]
            // `layer` closed their interceptor stream.
            LayerUdpOutgoing::Close(LayerClose { ref connection_id }) => {
                self.writers.remove(connection_id);
                self.readers.remove(connection_id);
                UDP_OUTGOING_CONNECTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                Ok(())
            }
        }
    }
}

type UdpReadStream = ThrottledStream<UdpReadHalf>;

/// Handles (briefly) the `UdpOutgoingRequest` and `UdpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct UdpOutgoingApi {
    task_status: BgTaskStatus,
    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<LayerUdpOutgoing>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Throttled<DaemonUdpOutgoing>>,
}

impl UdpOutgoingApi {
    pub(crate) fn new(runtime: &BgTaskRuntime) -> Self {
        // IMPORTANT: this makes tokio tasks spawn on `runtime`.
        // Do not remove this.
        let _rt = runtime.handle().enter();

        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task_status =
            tokio::spawn(UdpOutgoingTask::new(runtime.target_pid(), layer_rx, daemon_tx).run())
                .into_status("UdpOutgoingTask");

        Self {
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    /// Sends a `UdpOutgoingRequest` to the `interceptor_task`.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn send_to_task(&mut self, message: LayerUdpOutgoing) -> AgentResult<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }

    /// Receives a `UdpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn recv_from_task(&mut self) -> AgentResult<Throttled<DaemonUdpOutgoing>> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.wait_assert_running().await),
        }
    }
}

struct UdpReadHalf {
    socket: Arc<UdpSocket>,
    buffer: BytesMut,
}

impl From<Arc<UdpSocket>> for UdpReadHalf {
    fn from(socket: Arc<UdpSocket>) -> Self {
        Self {
            socket,
            // 64kb is the maximal possible size of a UDP packet.
            buffer: BytesMut::with_capacity(64 * 1024),
        }
    }
}

impl Stream for UdpReadHalf {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.buffer.clear();
        let mut read = ReadBuf::uninit(this.buffer.spare_capacity_mut());
        std::task::ready!(this.socket.poll_recv(cx, &mut read))?;
        Poll::Ready(Some(Ok(read.filled().to_vec().into())))
    }
}
