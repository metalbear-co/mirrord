use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    thread,
};

use bytes::BytesMut;
use futures::{
    prelude::*,
    stream::{SplitSink, SplitStream},
};
use kameo::actor::ActorRef;
use mirrord_protocol::{
    outgoing::{udp::*, *},
    ConnectionId, ResponseError,
};
use streammap_ext::StreamMap;
use tokio::{
    net::UdpSocket,
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::{codec::BytesCodec, udp::UdpFramed};
use tracing::{debug, trace, warn};

use super::MetricsActor;
use crate::{
    error::Result,
    metrics::outgoing_traffic::{MetricsDecUdpOutgoingConnection, MetricsIncUdpOutgoingConnection},
    util::run_thread_in_namespace,
    watched_task::{TaskStatus, WatchedTask},
};

type Layer = LayerUdpOutgoing;
type Daemon = DaemonUdpOutgoing;

/// Handles (briefly) the `UdpOutgoingRequest` and `UdpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct UdpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<()>,

    /// Status of the `interceptor_task`.
    task_status: TaskStatus,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<Layer>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Daemon>,
}

/// Performs an [`UdpSocket::connect`] that handles 3 situations:
///
/// 1. Normal `connect` called on an udp socket by the user, through the [`LayerConnect`] message;
/// 2. DNS special-case connection that comes on port `53`, where we have a hack that fakes a
///    connected udp socket. This case in particular requires that the user enable file ops with
///    read access to `/etc/resolv.conf`, otherwise they'll be getting a mismatched connection;
/// 3. User is trying to use `sendto` and `recvfrom`, we use the same hack as in DNS to fake a
///    connection.
#[tracing::instrument(level = "trace", ret)]
async fn connect(remote_address: SocketAddr) -> Result<UdpSocket, ResponseError> {
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

    pub(crate) fn new(pid: Option<u64>, metrics: ActorRef<MetricsActor>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let watched_task = WatchedTask::new(
            Self::TASK_NAME,
            Self::interceptor_task(layer_rx, daemon_tx, metrics),
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

    /// The [`UdpOutgoingApi`] task.
    ///
    /// Receives [`LayerUdpOutgoing`] messages and replies with [`DaemonUdpOutgoing`].
    #[allow(clippy::type_complexity)]
    async fn interceptor_task(
        mut layer_rx: Receiver<Layer>,
        daemon_tx: Sender<Daemon>,
        metrics: ActorRef<MetricsActor>,
    ) -> Result<()> {
        let mut connection_ids = 0..=ConnectionId::MAX;

        // TODO: Right now we're manually keeping these 2 maps in sync (aviram suggested using
        // `Weak` for `writers`).
        let mut writers: HashMap<
            ConnectionId,
            (
                SplitSink<UdpFramed<BytesCodec>, (BytesMut, SocketAddr)>,
                SocketAddr,
            ),
        > = HashMap::default();

        let mut readers: StreamMap<ConnectionId, SplitStream<UdpFramed<BytesCodec>>> =
            StreamMap::default();

        loop {
            select! {
                biased;

                // [layer] -> [agent]
                Some(layer_message) = layer_rx.recv() => {
                    trace!("udp: interceptor_task -> layer_message {:?}", layer_message);
                    match layer_message {
                        // [user] -> [layer] -> [agent] -> [layer]
                        // `user` is asking us to connect to some remote host.
                        LayerUdpOutgoing::Connect(LayerConnect { remote_address }) => {
                            let daemon_connect = connect(remote_address.clone().try_into()?)
                                    .await
                                    .and_then(|mirror_socket| {
                                        let connection_id = connection_ids
                                            .next()
                                            .ok_or_else(|| ResponseError::IdsExhausted("connect".into()))?;

                                        debug!("interceptor_task -> mirror_socket {:#?}", mirror_socket);

                                        let peer_address = mirror_socket.peer_addr()?;
                                        let local_address = mirror_socket.local_addr()?;
                                        let local_address = SocketAddress::Ip(local_address);

                                        let framed = UdpFramed::new(mirror_socket, BytesCodec::new());
                                        debug!("interceptor_task -> framed {:#?}", framed);

                                        let (sink, stream): (
                                            SplitSink<UdpFramed<BytesCodec>, (BytesMut, SocketAddr)>,
                                            SplitStream<UdpFramed<BytesCodec>>,
                                        ) = framed.split();

                                        writers.insert(connection_id, (sink, peer_address));
                                        readers.insert(connection_id, stream);

                                        Ok(DaemonConnect {
                                            connection_id,
                                            remote_address,
                                            local_address
                                        })
                                    });

                            let daemon_message = DaemonUdpOutgoing::Connect(daemon_connect);
                            debug!("interceptor_task -> daemon_message {:#?}", daemon_message);

                            daemon_tx.send(daemon_message).await?;

                            let _ = metrics
                                .tell(MetricsIncUdpOutgoingConnection)
                                .await
                                .inspect_err(|fail| tracing::warn!(%fail, "agent metrics failure!"));
                        }
                        // [user] -> [layer] -> [agent] -> [remote]
                        // `user` wrote some message to the remote host.
                        LayerUdpOutgoing::Write(LayerWrite {
                            connection_id,
                            bytes,
                        }) => {
                            let daemon_write = match writers
                                .get_mut(&connection_id)
                                .ok_or(ResponseError::NotFound(connection_id))
                            {
                                Ok((mirror, remote_address)) => mirror
                                    .send((BytesMut::from(bytes.as_slice()), *remote_address))
                                    .await
                                    .map_err(ResponseError::from),
                                Err(fail) => Err(fail),
                            };

                            if let Err(fail) = daemon_write {
                                warn!("LayerUdpOutgoing::Write -> Failed with {:#?}", fail);
                                writers.remove(&connection_id);
                                readers.remove(&connection_id);

                                let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                                daemon_tx.send(daemon_message).await?;

                                let _ = metrics
                                    .tell(MetricsDecUdpOutgoingConnection)
                                    .await
                                    .inspect_err(|fail| tracing::warn!(%fail, "agent metrics failure!"));
                            }
                        }
                        // [layer] -> [agent]
                        // `layer` closed their interceptor stream.
                        LayerUdpOutgoing::Close(LayerClose { ref connection_id }) => {
                            writers.remove(connection_id);
                            readers.remove(connection_id);

                            let _ = metrics
                                .tell(MetricsDecUdpOutgoingConnection)
                                .await
                                .inspect_err(|fail| tracing::warn!(%fail, "agent metrics failure!"));
                        }
                    }
                }

                // [remote] -> [agent] -> [layer] -> [user]
                // Read the data from one of the connected remote hosts, and forward the result back
                // to the `user`.
                Some((connection_id, remote_read)) = readers.next() => {
                    trace!("interceptor_task -> read connection_id {:#?}", connection_id);

                    match remote_read {
                        Some(read) => {
                            let daemon_read = read
                                .map_err(ResponseError::from)
                                .map(|(bytes, _)| DaemonRead { connection_id, bytes: bytes.to_vec() });

                            let daemon_message = DaemonUdpOutgoing::Read(daemon_read);
                            daemon_tx.send(daemon_message).await?
                        }
                        None => {
                            trace!("interceptor_task -> close connection {:#?}", connection_id);
                            writers.remove(&connection_id);
                            readers.remove(&connection_id);

                            let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                            daemon_tx.send(daemon_message).await?;

                            let _ = metrics
                                .tell(MetricsDecUdpOutgoingConnection)
                                .await
                                .inspect_err(|fail| tracing::warn!(%fail, "agent metrics failure!"));
                        }
                    }
                }
                else => {
                    // We have no more data coming from any of the remote hosts.
                    warn!("interceptor_task -> no messages left");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Sends a `UdpOutgoingRequest` to the `interceptor_task`.
    pub(crate) async fn layer_message(&mut self, message: LayerUdpOutgoing) -> Result<()> {
        trace!(
            "UdpOutgoingApi::layer_message -> layer_message {:#?}",
            message
        );

        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Receives a `UdpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn daemon_message(&mut self) -> Result<DaemonUdpOutgoing> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.unwrap_err().await),
        }
    }
}
