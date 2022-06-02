use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
};

use actix_codec::{AsyncRead, AsyncWrite};
use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use mirrord_protocol::{
    ClientCodec, ClientMessage, ConnectionID, DaemonMessage, TCPClose, TCPData,
};
use tokio::{
    io::{copy_bidirectional, duplex, split, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf},
    net::TcpStream,
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use tokio_stream::StreamMap;
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};

mod common;
mod config;
mod macros;
mod pod_api;
mod sockets;
mod steal;
mod tcp;

use tracing_subscriber::prelude::*;

use crate::{
    common::{HookMessage, Port},
    config::Config,
    sockets::{SocketInformation, CONNECTION_QUEUE},
};

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
}

pub static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;

#[derive(Debug)]
enum TcpTunnelMessages {
    Data(Vec<u8>),
    Close,
}

#[derive(Debug, Clone)]
struct ListenData {
    ipv6: bool,
    port: Port,
    fd: RawFd,
}

// TODO: We can probably drop the tcptunnelmessage close and just drop the sender, would make code
// simpler.
async fn tcp_tunnel(mut local_stream: TcpStream, mut receiver: Receiver<TcpTunnelMessages>) {
    loop {
        select! {
            message = receiver.recv() => {
                match message {
                    Some(TcpTunnelMessages::Data(data)) => {
                        local_stream.write_all(&data).await.unwrap()
                    },
                    Some(TcpTunnelMessages::Close) => break,
                    None => break
                };
            },
            _ = local_stream.readable() => {
                let mut data = vec![0; 1024];
                match local_stream.try_read(&mut data) {
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        continue
                        },
                    Err(err) => {
                        debug!("local stream ended with err {:?}", err);
                        break;
                    }
                    Ok(n) if n == 0 => break,
                    Ok(_) => {}
                }

            }
        }
    }
    debug!("exiting tcp tunnel");
}

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Initializing mirrord-layer!");

    let config = Config::init_from_env().unwrap();

    let pf = RUNTIME.block_on(pod_api::create_agent(
        &config.impersonated_pod_name,
        &config.impersonated_pod_namespace,
        &config.agent_namespace,
        config.agent_rust_log,
        config.agent_image.unwrap_or_else(|| {
            concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
        }),
    ));

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    enable_hooks();

    RUNTIME.spawn(poll_agent(pf, receiver, config.steal_traffic));
}

#[inline]
async fn handle_hook_message(
    hook_message: HookMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin, ClientCodec>,
    steal_traffic: bool,
) {
    match hook_message {
        HookMessage::Listen(listen_message) => {
            debug!("HookMessage::Listen {:?}", listen_message);
            let msg = if steal_traffic {
                ClientMessage::PortSteal(listen_message.real_port)
            } else {
                ClientMessage::PortSubscribe(vec![listen_message.real_port])
            };
            let _listen_data = codec.send(msg).await.map(|()| {
                port_mapping.insert(
                    listen_message.real_port,
                    ListenData {
                        port: listen_message.fake_port,
                        ipv6: listen_message.ipv6,
                        fd: listen_message.fd,
                    },
                )
            });
        }
    }
}

#[inline]
async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    active_connections: &mut HashMap<ConnectionID, Sender<TcpTunnelMessages>>,
    stolen_reads: &mut StreamMap<ConnectionID, ReaderStream<ReadHalf<DuplexStream>>>,
    stolen_writes: &mut HashMap<ConnectionID, WriteHalf<DuplexStream>>,
) {
    match &daemon_message {
        DaemonMessage::NewTCPConnection(conn) | DaemonMessage::NewStolenConnection(conn) => {
            debug!("DaemonMessage::NewTCPConnection {conn:#?}");
            let _ = port_mapping
                .get(&conn.destination_port)
                .map(|listen_data| {
                    let addr = match listen_data.ipv6 {
                        false => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen_data.port),
                        true => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen_data.port),
                    };

                    let info =
                        SocketInformation::new(SocketAddr::new(conn.address, conn.source_port));
                    {
                        CONNECTION_QUEUE.lock().unwrap().add(&listen_data.fd, info);
                    }

                    TcpStream::connect(addr)
                })
                .map(|stream| {
                    match daemon_message {
                        DaemonMessage::NewTCPConnection(_) => {
                            let (sender, receiver) = channel::<TcpTunnelMessages>(1000);
                            active_connections.insert(conn.connection_id, sender);
                            task::spawn(async move {
                                tcp_tunnel(stream.await.unwrap(), receiver).await;
                            });
                        }
                        DaemonMessage::NewStolenConnection(_) => {
                            let (mut a, b) = duplex(1024);
                            let (read_half, write_half) = split(b);
                            stolen_writes.insert(conn.connection_id, write_half);
                            stolen_reads.insert(conn.connection_id, ReaderStream::new(read_half));
                            task::spawn(async move {
                                let mut stream = stream.await.unwrap();
                                copy_bidirectional(&mut a, &mut stream).await.unwrap();
                            });
                        }
                        _ => unreachable!("can't get here"),
                    };
                });
        }
        DaemonMessage::TCPData(msg) => {
            debug!("Received data from connection id {}", msg.connection_id);
            let connection = active_connections.get(&msg.connection_id);
            if connection.is_none() {
                debug!("Connection {} not found", msg.connection_id);
                return;
            }
            if let Err(fail) = connection
                .map(|sender| sender.send(TcpTunnelMessages::Data(msg.data.clone())))
                .unwrap()
                .await
            {
                error!("DaemonMessage::TCPData error {fail:#?}");
                active_connections.remove(&msg.connection_id);
            }
        }
        DaemonMessage::TCPClose(msg) => {
            debug!("Closing connection {}", msg.connection_id);
            // TODO: This should be take.. no?
            if let Err(fail) = active_connections
                .get(&msg.connection_id)
                .map(|sender| sender.send(TcpTunnelMessages::Close))
                .unwrap()
                .await
            {
                error!("DaemonMessage::TCPClose error {fail:#?}");
                active_connections.remove(&msg.connection_id);
            }
        }
        DaemonMessage::Close => todo!(),
        DaemonMessage::LogMessage(_) => todo!(),
        DaemonMessage::StolenTCPData(msg) => {
            debug!("Received data from connection id {}", msg.connection_id);
            let connection = stolen_writes.get_mut(&msg.connection_id);
            if connection.is_none() {
                debug!("Connection {} not found", msg.connection_id);
                return;
            }
            if let Err(fail) = connection
                .map(|sender| sender.write_all(&msg.data))
                .unwrap()
                .await
            {
                error!("DaemonMessage::StolenTCPData error {fail:#?}");
                stolen_reads.remove(&msg.connection_id);
                stolen_writes.remove(&msg.connection_id);
            }
        }
        DaemonMessage::StolenTCPClose(msg) => {
            debug!("Closing connection {}", msg.connection_id);
            if stolen_writes.remove(&msg.connection_id).is_none() {
                warn!("Connection wasn't found {:?}", msg.connection_id);
            }
            if stolen_reads.remove(&msg.connection_id).is_none() {
                warn!("Connection wasn't found {:?}", msg.connection_id);
            }
        }
    }
}

async fn poll_agent(
    mut pf: Portforwarder,
    mut receiver: Receiver<HookMessage>,
    steal_traffic: bool,
) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable

    // `codec` is used to retrieve messages from the daemon (messages that are sent from -agent to
    // -layer)
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut port_mapping: HashMap<Port, ListenData> = HashMap::new();
    let mut active_connections = HashMap::new();
    let mut stolen_reads = StreamMap::new();
    let mut stolen_writes = HashMap::new();
    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(), &mut port_mapping, &mut codec, steal_traffic).await;
            },
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(), &mut port_mapping, &mut active_connections, &mut stolen_reads, &mut stolen_writes).await;
            },
            Some((connection_id, message)) = stolen_reads.next() => {
                debug!("{message:?}");
                match message {
                    Err(err) => {

                    }
                    // Err(err) => {
                    //     debug!("connection ended {connection_id:?}");
                    //     stolen_reads.remove(&connection_id);
                    //     stolen_writes.remove(&connection_id);
                    //     codec.send(ClientMessage::CloseStolenConnection(TCPClose {
                    //         connection_id
                    //     })).await;
                    // },
                    // Some(data) => {
                    //     codec.send(ClientMessage::StolenTCPData(TCPData {connection_id, data: data.to_vec()})).await;
                    // }

                }

            }
        };
    }
}

fn enable_hooks() {
    let interceptor = Interceptor::obtain(&GUM);
    sockets::enable_hooks(interceptor)
}
