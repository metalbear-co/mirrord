use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::LazyLock,
    thread,
};

use bytes::BytesMut;
use futures::{
    prelude::*,
    stream::{SplitSink, SplitStream},
};
use mirrord_protocol::{
    outgoing::{udp::*, *},
    ConnectionId, RemoteError, ResponseError, SendRecvRequest, SendRecvResponse,
};
use regex::Regex;
use streammap_ext::StreamMap;
use tokio::{
    fs::File,
    io::AsyncReadExt,
    net::UdpSocket,
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::{codec::BytesCodec, udp::UdpFramed};
use tracing::{debug, trace, warn};

use crate::{
    error::{AgentError, Result},
    util::{run_thread_in_namespace, IndexAllocator},
};

type Layer = LayerUdpOutgoing;
type Daemon = DaemonUdpOutgoing;

static NAMESERVER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^nameserver.*").unwrap());
const DNS_PORT: u16 = 53;

/// Handles (briefly) the `UdpOutgoingRequest` and `UdpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct UdpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<Result<()>>,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<Layer>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Daemon>,
}

async fn resolve_dns() -> Result<SocketAddr, ResponseError> {
    trace!("resolve_dns -> ");

    let mut resolv_conf_contents = String::with_capacity(1024);
    let mut resolv_conf = File::open("/etc/resolv.conf").await?;

    resolv_conf
        .read_to_string(&mut resolv_conf_contents)
        .await?;

    let nameserver = resolv_conf_contents
        .lines()
        .find(|line| NAMESERVER.is_match(line))
        .ok_or(RemoteError::NameserverNotFound)?
        .split_whitespace()
        .last()
        .ok_or(RemoteError::NameserverNotFound)?;

    let dns_address: SocketAddr = format!("{nameserver}:{DNS_PORT}")
        .parse()
        .map_err(RemoteError::from)?;

    Ok(dns_address)
}

async fn connect(remote_address: SocketAddr) -> Result<UdpSocket, ResponseError> {
    trace!("connect -> remote_address {:#?}", remote_address);

    let remote_address = if remote_address.port() == DNS_PORT {
        resolve_dns().await?
    } else {
        remote_address
    };

    let mirror_address = match remote_address {
        std::net::SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        std::net::SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };

    let mirror_socket = UdpSocket::bind(mirror_address).await?;
    mirror_socket.connect(remote_address).await?;

    Ok(mirror_socket)
}

impl UdpOutgoingApi {
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task = run_thread_in_namespace(
            Self::interceptor_task(layer_rx, daemon_tx),
            "UdpOutgoing".to_string(),
            pid,
            "net",
        );

        Self {
            _task: task,
            layer_tx,
            daemon_rx,
        }
    }

    /// Does the actual work for `Request`s and prepares the `Responses:
    #[allow(clippy::type_complexity)]
    async fn interceptor_task(
        mut layer_rx: Receiver<Layer>,
        daemon_tx: Sender<Daemon>,
    ) -> Result<()> {
        let mut allocator = IndexAllocator::<ConnectionId>::default();

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
                            let daemon_connect = connect(remote_address)
                                    .await
                                    .map(|mirror_socket| {
                                        let connection_id = allocator
                                            .next_index()
                                            .ok_or_else(|| ResponseError::AllocationFailure("interceptor_task".to_string()))
                                            .unwrap() as ConnectionId;

                                        debug!("interceptor_task -> mirror_socket {:#?}", mirror_socket);
                                        let peer_address = mirror_socket.peer_addr().unwrap();

                                        let framed = UdpFramed::new(mirror_socket, BytesCodec::new());
                                        debug!("interceptor_task -> framed {:#?}", framed);
                                        let (sink, stream): (
                                            SplitSink<UdpFramed<BytesCodec>, (BytesMut, SocketAddr)>,
                                            SplitStream<UdpFramed<BytesCodec>>,
                                        ) = framed.split();

                                        writers.insert(connection_id, (sink, peer_address));
                                        readers.insert(connection_id, stream);

                                        DaemonConnect {
                                            connection_id,
                                            remote_address,
                                        }
                                    });

                            let daemon_message = DaemonUdpOutgoing::Connect(daemon_connect);
                            debug!("interceptor_task -> daemon_message {:#?}", daemon_message);
                            daemon_tx.send(daemon_message).await?
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
                                daemon_tx.send(daemon_message).await?
                            }
                        }
                        // [layer] -> [agent]
                        // `layer` closed their interceptor stream.
                        LayerUdpOutgoing::Close(LayerClose { ref connection_id }) => {
                            writers.remove(connection_id);
                            readers.remove(connection_id);
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
                            daemon_tx.send(daemon_message).await?
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
        Ok(self.layer_tx.send(message).await?)
    }

    /// Receives a `UdpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn daemon_message(&mut self) -> Result<DaemonUdpOutgoing> {
        self.daemon_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}

pub(crate) struct SendRecvManager {
    open_sockets: HashMap<UdpSocket, SocketAddr>,
}

impl SendRecvManager {
    pub(crate) fn new() -> Self {
        Self {
            open_sockets: HashMap::new(),
        }
    }

    pub(crate) async fn handle_message(
        &mut self,
        request: SendRecvRequest,
    ) -> Result<Option<SendRecvResponse>> {
        match request {
            SendRecvRequest::SendMsg(SendMsgRequest {
                message,
                addr,
                bound,
            }) => {
                let udp_socket = if let Some(bound_address) = bound {
                    UdpSocket::bind(bound_address.address).await?
                } else {
                    UdpSocket::bind("0.0.0.0:0").await?
                };

                let sent_amount = udp_socket.send_to(&message.as_bytes(), addr).await?;

                // some sort of mapping between the socket and the address

                Ok(Some(SendRecvResponse::SendMsg(SendMsgResponse {
                    sent_amount,
                })))
            }
        }
    }
}
