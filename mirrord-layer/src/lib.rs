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
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
    time::{sleep, Duration},
};
use tracing::{debug, error, info, trace};

mod common;
mod config;
mod macros;
mod pod_api;
mod sockets;
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

    RUNTIME.spawn(poll_agent(pf, receiver));
}

#[inline]
async fn handle_hook_message(
    hook_message: HookMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin, ClientCodec>,
) {
    match hook_message {
        HookMessage::Listen(listen_message) => {
            debug!("HookMessage::Listen {:?}", listen_message);

            let _listen_data = codec
                .send(ClientMessage::PortSubscribe(vec![listen_message.real_port]))
                .await
                .map(|()| {
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
    loop {
        codec.send(ClientMessage::Ping).await.unwrap();
        trace!("client sent ping");
        sleep(Duration::from_secs(30)).await;
    }
}

#[inline]
async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    active_connections: &mut HashMap<u16, Sender<TcpTunnelMessages>>,
) {
    match daemon_message {
        DaemonMessage::NewTCPConnection(conn) => {
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
                    let (sender, receiver) = channel::<TcpTunnelMessages>(1000);

                    active_connections.insert(conn.connection_id, sender);

                    task::spawn(async move { tcp_tunnel(stream.await.unwrap(), receiver).await })
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
                .map(|sender| sender.send(TcpTunnelMessages::Data(msg.data)))
                .unwrap()
                .await
            {
                error!("DaemonMessage::TCPData error {fail:#?}");
                active_connections.remove(&msg.connection_id);
            }
        }
        DaemonMessage::TCPClose(msg) => {
            debug!("Closing connection {}", msg.connection_id);
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
        DaemonMessage::Pong => trace!("Daemon sent pong!"),
        DaemonMessage::Close => todo!(),
        DaemonMessage::LogMessage(_) => todo!(),
    }
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<HookMessage>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable

    // `codec` is used to retrieve messages from the daemon (messages that are sent from -agent to
    // -layer)
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut port_mapping: HashMap<Port, ListenData> = HashMap::new();
    let mut active_connections = HashMap::new();
    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(), &mut port_mapping, &mut codec).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(), &mut port_mapping, &mut active_connections).await;
            }
        }
    }
}

fn enable_hooks() {
    let interceptor = Interceptor::obtain(&GUM);
    sockets::enable_hooks(interceptor)
}
