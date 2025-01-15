use core::fmt;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    thread,
};

use bytes::{Bytes, BytesMut};
use futures::{
    prelude::*,
    stream::{SplitSink, SplitStream},
};
use mirrord_protocol::{
    outgoing::{udp::*, *},
    ConnectionId, ResponseError,
};
use streammap_ext::StreamMap;
use tokio::{
    io,
    net::UdpSocket,
    select,
    sync::mpsc::{self, error::SendError, Receiver, Sender},
};
use tokio_util::{codec::BytesCodec, udp::UdpFramed};
use tracing::Level;

use crate::{
    error::AgentResult,
    metrics::UDP_OUTGOING_CONNECTION,
    util::run_thread_in_namespace,
    watched_task::{TaskStatus, WatchedTask},
};

/// Task that handles [`LayerUdpOutgoing`] and [`DaemonUdpOutgoing`] messages.
///
/// We start these tasks from the [`UdpOutgoingApi`] as a [`WatchedTask`].
struct UdpOutgoingTask {
    next_connection_id: ConnectionId,
    /// Writing halves of peer connections made on layer's requests.
    writers: HashMap<
        ConnectionId,
        (
            SplitSink<UdpFramed<BytesCodec>, (BytesMut, SocketAddr)>,
            SocketAddr,
        ),
    >,
    /// Reading halves of peer connections made on layer's requests.
    readers: StreamMap<ConnectionId, SplitStream<UdpFramed<BytesCodec>>>,
    /// Optional pid of agent's target. Used in [`SocketStream::connect`].
    pid: Option<u64>,
    layer_rx: Receiver<LayerUdpOutgoing>,
    daemon_tx: Sender<DaemonUdpOutgoing>,
}

impl Drop for UdpOutgoingTask {
    fn drop(&mut self) {
        let connections = self.readers.keys().chain(self.writers.keys()).count();
        UDP_OUTGOING_CONNECTION.sub(connections as i64);
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
    fn new(
        pid: Option<u64>,
        layer_rx: Receiver<LayerUdpOutgoing>,
        daemon_tx: Sender<DaemonUdpOutgoing>,
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
    pub(super) async fn run(mut self) -> AgentResult<()> {
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
                    self.handle_connection_read(connection_id, remote_read.transpose().map(|remote| remote.map(|(read, _)| read.into()))).await.is_err()
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
    ) -> AgentResult<(), SendError<DaemonUdpOutgoing>> {
        match read {
            Ok(Some(read)) => {
                let message = DaemonUdpOutgoing::Read(Ok(DaemonRead {
                    connection_id,
                    bytes: read.to_vec(),
                }));

                self.daemon_tx.send(message).await?
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
                UDP_OUTGOING_CONNECTION.dec();

                let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                self.daemon_tx.send(daemon_message).await?;
            }
            Ok(None) => {
                self.writers.remove(&connection_id);
                self.readers.remove(&connection_id);
                UDP_OUTGOING_CONNECTION.dec();

                let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                self.daemon_tx.send(daemon_message).await?;
            }
        }

        Ok(())
    }

    /// Returns [`Err`] only when the client has disconnected.
    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_layer_msg(
        &mut self,
        message: LayerUdpOutgoing,
    ) -> AgentResult<(), SendError<DaemonUdpOutgoing>> {
        match message {
            // [user] -> [layer] -> [agent] -> [layer]
            // `user` is asking us to connect to some remote host.
            LayerUdpOutgoing::Connect(LayerConnect { remote_address }) => {
                let daemon_connect =
                    connect(remote_address.clone())
                        .await
                        .and_then(|mirror_socket| {
                            let connection_id = self.next_connection_id;
                            self.next_connection_id += 1;

                            let peer_address = mirror_socket.peer_addr()?;
                            let local_address = mirror_socket.local_addr()?;
                            let local_address = SocketAddress::Ip(local_address);

                            let framed = UdpFramed::new(mirror_socket, BytesCodec::new());

                            let (sink, stream): (
                                SplitSink<UdpFramed<BytesCodec>, (BytesMut, SocketAddr)>,
                                SplitStream<UdpFramed<BytesCodec>>,
                            ) = framed.split();

                            self.writers.insert(connection_id, (sink, peer_address));
                            self.readers.insert(connection_id, stream);
                            UDP_OUTGOING_CONNECTION.inc();

                            Ok(DaemonConnect {
                                connection_id,
                                remote_address,
                                local_address,
                            })
                        });

                tracing::trace!(
                    result = ?daemon_connect,
                    "Connection attempt finished.",
                );

                self.daemon_tx
                    .send(DaemonUdpOutgoing::Connect(daemon_connect))
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
                        .send((BytesMut::from(bytes.as_slice()), *remote_address))
                        .await
                        .map_err(ResponseError::from),
                    Err(fail) => Err(fail),
                };

                match write_result {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        self.writers.remove(&connection_id);
                        self.readers.remove(&connection_id);
                        UDP_OUTGOING_CONNECTION.dec();

                        tracing::trace!(
                            connection_id,
                            ?error,
                            "Failed to handle layer write, sending close message to the client.",
                        );

                        let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                        self.daemon_tx.send(daemon_message).await?;

                        Ok(())
                    }
                }
            }
            // [layer] -> [agent]
            // `layer` closed their interceptor stream.
            LayerUdpOutgoing::Close(LayerClose { ref connection_id }) => {
                self.writers.remove(connection_id);
                self.readers.remove(connection_id);
                UDP_OUTGOING_CONNECTION.dec();

                Ok(())
            }
        }
    }
}

/// Handles (briefly) the `UdpOutgoingRequest` and `UdpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct UdpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<()>,

    /// Status of the `interceptor_task`.
    task_status: TaskStatus,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<LayerUdpOutgoing>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<DaemonUdpOutgoing>,
}

/// Performs an [`UdpSocket::connect`] that handles 3 situations:
///
/// 1. Normal `connect` called on an udp socket by the user, through the [`LayerConnect`] message;
/// 2. DNS special-case connection that comes on port `53`, where we have a hack that fakes a
///    connected udp socket. This case in particular requires that the user enable file ops with
///    read access to `/etc/resolv.conf`, otherwise they'll be getting a mismatched connection;
/// 3. User is trying to use `sendto` and `recvfrom`, we use the same hack as in DNS to fake a
///    connection.
#[tracing::instrument(level = Level::TRACE, ret, err(level = Level::DEBUG))]
async fn connect(remote_address: SocketAddress) -> AgentResult<UdpSocket, ResponseError> {
    let remote_address = remote_address.try_into()?;
    let mirror_address = match remote_address {
        std::net::SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        std::net::SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };

    let mirror_socket = UdpSocket::bind(mirror_address).await?;
    mirror_socket.connect(remote_address).await?;

    Ok(mirror_socket)
}

impl UdpOutgoingApi {
    const TASK_NAME: &'static str = "UdpOutgoing";

    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let watched_task = WatchedTask::new(
            Self::TASK_NAME,
            UdpOutgoingTask::new(pid, layer_rx, daemon_tx).run(),
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

    /// Sends a `UdpOutgoingRequest` to the `interceptor_task`.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn send_to_task(&mut self, message: LayerUdpOutgoing) -> AgentResult<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Receives a `UdpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn recv_from_task(&mut self) -> AgentResult<DaemonUdpOutgoing> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.unwrap_err().await),
        }
    }
}
