#![feature(once_cell)]

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    thread,
    time::Duration,
};

use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use libc::{c_char, c_int, c_void, sockaddr, socklen_t};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use tracing::{debug, error};
use tracing_subscriber::{prelude::*, util::SubscriberInitExt};

mod common;
mod config;
mod file;
mod macros;
mod pod_api;
mod sockets;

use crate::{
    common::{HookMessage, Port},
    config::Config,
    file::FILE_HOOK_SENDER,
    sockets::{SocketInformation, CONNECTION_QUEUE},
};

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    pub(crate) static ref RUNTIME: Runtime = Runtime::new().unwrap();
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

    debug!("init called");
    let config = Config::init_from_env().unwrap();
    let pf = RUNTIME.block_on(pod_api::create_agent(
        &config.impersonated_pod_name,
        &config.impersonated_pod_namespace,
        &config.agent_namespace,
        config.agent_rust_log,
        config.agent_image,
    ));
    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };
    enable_hooks();
    RUNTIME.spawn(poll_agent(pf, receiver));
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<HookMessage>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut port_mapping: HashMap<Port, ListenData> = HashMap::new();
    let mut active_connections = HashMap::new();
    loop {
        select! {
            message = receiver.recv() => {
                match message {
                    Some(HookMessage::Listen(listen)) => {
                        debug!("poll_agent -> received `Listen` message from hook {:?}", listen);
                        codec.send(ClientMessage::PortSubscribe(vec![listen.real_port])).await.unwrap();
                        port_mapping.insert(listen.real_port, ListenData{port: listen.fake_port, ipv6: listen.ipv6, fd: listen.fd});
                    }
                    Some(HookMessage::OpenFile(open)) => {
                        debug!("poll_agent -> received `Open` message from hook {:?}", open);
                        codec.send(ClientMessage::OpenFileRequest(open.path)).await.unwrap();
                    }
                    None => {
                        debug!("NONE in recv");
                        break
                    }
                }
            }
            message = codec.next() => {
                match message {
                    Some(Ok(DaemonMessage::NewTCPConnection(conn))) => {
                        debug!("new connection {:?}", conn);
                        let listen_data = match port_mapping.get(&conn.destination_port) {
                            Some(listen_data) => (*listen_data).clone(),
                            None => {
                                debug!("no listen_data for {:?}", conn.destination_port);
                                continue;
                            }
                        };
                        let addr = match listen_data.ipv6 {
                            false => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen_data.port),
                            true => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen_data.port),
                        };
                        let info = SocketInformation::new(SocketAddr::new(conn.address, conn.source_port));
                        {
                            CONNECTION_QUEUE.lock().unwrap().add(&listen_data.fd, info);
                        }
                        let stream = match TcpStream::connect(addr).await {
                            Ok(stream) => stream,
                            Err(err) => {
                                error!("failed to connect to port {:?}", err);
                                continue;
                            }
                        };
                        let (sender, receiver) = channel::<TcpTunnelMessages>(1000);
                        active_connections.insert(conn.connection_id, sender);
                        task::spawn(async move {
                            tcp_tunnel(stream, receiver).await
                        });
                    }
                    Some(Ok(DaemonMessage::TCPData(msg))) => {
                        let sender = match active_connections.get(&msg.connection_id) {
                            Some(sender) => sender,
                            None => {
                                debug!("no sender for {:?}", msg.connection_id);
                                continue;
                            }
                        };
                        if let Err(err) = sender.send(TcpTunnelMessages::Data(msg.data)).await {
                                debug!("sender error {:?}", err);
                                active_connections.remove(&msg.connection_id);
                        }
                    },
                    Some(Ok(DaemonMessage::TCPClose(msg))) => {
                        let sender = match active_connections.remove(&msg.connection_id) {
                            Some(sender) => sender,
                            None => {
                                debug!("no sender for {:?}", msg.connection_id);
                                continue;
                            }
                        };
                        if let Err(err) = sender.send(TcpTunnelMessages::Close).await {
                                debug!("sender error {:?}", err);
                        }
                    }
                    Some(Ok(DaemonMessage::FileOpenResponse(file_open_message))) => {
                        debug!("poll_agent -> received a FileOpen message with contents {file_open_message:?}!");
                        unsafe { FILE_HOOK_SENDER.lock().unwrap().pop().unwrap().send(file_open_message) };
                    },
                    Some(_) => {
                        debug!("NONE in some");
                        break
                    },
                    None => {
                        thread::sleep(Duration::from_millis(2000));
                        debug!("NONE in none");
                        continue
                    }
                }
            }
        }
    }
}

fn enable_hooks() {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor.begin_transaction();

    sockets::enable_socket_hooks(&mut interceptor);
    file::enable_file_hooks(&mut interceptor);

    interceptor.end_transaction();
}
