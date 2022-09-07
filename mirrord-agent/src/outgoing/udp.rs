use std::{
    collections::HashMap,
    convert::{TryFrom, TryInto},
    net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
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
    ConnectionId, RemoteError, ResponseError,
};
use regex::Regex;
use socket2::{Domain, Protocol, SockAddr, Type};
use streammap_ext::StreamMap;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        UdpSocket,
    },
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::{
    codec::{BytesCodec, Decoder, Encoder},
    io::ReaderStream,
    udp::UdpFramed,
};
use tracing::{debug, info, trace, warn};

use crate::{
    error::AgentError,
    runtime::set_namespace,
    util::{run_thread, IndexAllocator},
};

type Layer = LayerUdpOutgoing;
type Daemon = DaemonUdpOutgoing;

static NAMESERVER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^nameserver.*").unwrap());

/// Handles (briefly) the `UdpOutgoingRequest` and `UdpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct UdpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<Result<(), AgentError>>,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<Layer>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Daemon>,
}

const DNS_PORT: u16 = 53;

async fn resolve_dns() -> Result<SocketAddr, ResponseError> {
    trace!("resolve_dns -> ");

    let mut resolv_conf_contents = String::with_capacity(1024);
    let mut resolv_conf = File::open("/etc/resolv.conf").await?;

    resolv_conf
        .read_to_string(&mut resolv_conf_contents)
        .await?;

    let nameserver = resolv_conf_contents
        .lines()
        .filter(|line| NAMESERVER.is_match(line))
        .next()
        .ok_or(RemoteError::NameserverNotFound)?
        .split_whitespace()
        .last()
        .ok_or(RemoteError::NameserverNotFound)?;

    let dns_address: SocketAddr = format!("{}:{}", nameserver, DNS_PORT)
        .parse()
        .map_err(|fail: AddrParseError| RemoteError::from(fail))?;

    info!("resolve_dns -> dns_address {:#?}", dns_address);

    Ok(dns_address)
}

// TODO(alex) [high] 2022-09-07: Special-case the port 53 DNS request, otherwise we don't need most
// of what is going on here.
async fn connect_clean(remote_address: SocketAddr) -> Result<UdpSocket, ResponseError> {
    trace!("connect -> remote_address {:#?}", remote_address);

    let remote_address = if remote_address.port() == 53 {
        resolve_dns().await?
    } else {
        remote_address
    };

    let mirror_address = match remote_address {
        std::net::SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        std::net::SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    info!("connect_clean -> mirror_address {:#?}", mirror_address);

    let mirror_socket = UdpSocket::bind(mirror_address).await;
    info!("connect -> bind {:#?}", mirror_socket);
    let mirror_socket = mirror_socket?;

    info!(
        "connect_clean -> connect {:#?}",
        mirror_socket.connect(remote_address).await
    );

    info!("connect_clean -> connected to {:#?}", remote_address);

    Ok(mirror_socket)
}

// TODO(alex) [high] 2022-09-07: Special-case the port 53 DNS request, otherwise we don't need most
// of what is going on here.
async fn connect_dirty(
    domain: i32,
    type_: i32,
    protocol: i32,
    remote_address: SocketAddr,
) -> Result<UdpSocket, ResponseError> {
    trace!("connect_dirty -> remote_address {:#?}", remote_address);

    let remote_address = if remote_address.port() == 53 {
        resolve_dns().await?
    } else {
        remote_address
    };

    let raw_socket = socket2::Socket::new_raw(
        Domain::from(domain),
        Type::from(type_),
        Some(Protocol::from(protocol)),
    )?;

    raw_socket.connect(&SockAddr::from(remote_address))?;

    let mirror_socket = UdpSocket::from_std(raw_socket.into())?;
    Ok(mirror_socket)
}

impl UdpOutgoingApi {
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task = run_thread(Self::interceptor_task(pid, layer_rx, daemon_tx));

        Self {
            _task: task,
            layer_tx,
            daemon_rx,
        }
    }

    /// Does the actual work for `Request`s and prepares the `Responses:
    async fn interceptor_task(
        pid: Option<u64>,
        mut layer_rx: Receiver<Layer>,
        daemon_tx: Sender<Daemon>,
    ) -> Result<(), AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        let mut allocator = IndexAllocator::default();

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
                        LayerUdpOutgoing::Connect(LayerUdpConnect { domain, type_, protocol, remote_address }) => {

                            // info!("connect_clean result {:#?}", connect_clean(remote_address).await);

                            let daemon_connect = connect_dirty(domain, type_, protocol, remote_address)
                            // let daemon_connect = connect_clean(remote_address)
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
                                .ok_or(ResponseError::NotFound(connection_id as usize))
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
                            writers.remove(&connection_id);
                            readers.remove(&connection_id);
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
                                .map(|(bytes, remote_address)| DaemonRead { connection_id, bytes: bytes.to_vec() });

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
    pub(crate) async fn layer_message(
        &mut self,
        message: LayerUdpOutgoing,
    ) -> Result<(), AgentError> {
        trace!(
            "UdpOutgoingApi::layer_message -> layer_message {:#?}",
            message
        );
        Ok(self.layer_tx.send(message).await?)
    }

    /// Receives a `UdpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn daemon_message(&mut self) -> Result<DaemonUdpOutgoing, AgentError> {
        self.daemon_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}
