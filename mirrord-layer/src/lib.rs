#![feature(once_cell)]

use std::{
    collections::HashMap,
    error::Error,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::Mutex,
    thread,
    time::Duration,
};

use actix_codec::{AsyncRead, AsyncWrite};
use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{channel::oneshot, SinkExt, StreamExt};
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

async fn handle_hook_message(
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin, ClientCodec>,
    open_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::FileOpenResponse>>>,
    port_mapping: &mut HashMap<Port, ListenData>,
    hook_message: Option<HookMessage>,
) {
    match hook_message {
        Some(HookMessage::Listen(listen)) => {
            debug!(
                "poll_agent -> received `Listen` message from hook {:?}",
                listen
            );
            codec
                .send(ClientMessage::PortSubscribe(vec![listen.real_port]))
                .await
                .unwrap();

            port_mapping.insert(
                listen.real_port,
                ListenData {
                    port: listen.fake_port,
                    ipv6: listen.ipv6,
                    fd: listen.fd,
                },
            );
        }
        Some(HookMessage::OpenFileHook(open)) => {
            // NOTE(alex): Lock the file handler and insert a channel that will be used to retrieve
            // the file data when it comes back from `DaemonMessage::OpenFileResponse`.
            debug!("poll_agent -> received `Open` message from hook {:?}", open);
            open_file_handler.lock().unwrap().push(open.file_channel_tx);

            codec
                .send(ClientMessage::OpenFileRequest(open.path))
                .await
                .unwrap();
        }
        None => {
            debug!("NONE in recv");
            // TODO(alex) [low] 2022-05-16: Removed a `break` statement here, so it's probably a
            // good idea to revisit this whole `match`.
        }
    }
}

async fn handle_daemon_message(
    open_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::FileOpenResponse>>>,
    port_mapping: &mut HashMap<Port, ListenData>,
    active_connections: &mut HashMap<u16, Sender<TcpTunnelMessages>>,
    daemon_message: DaemonMessage,
) {
    match daemon_message {
        DaemonMessage::NewTCPConnection(conn) => {
            debug!("new connection {:?}", conn);
            // TODO(alex) [high] 2022-05-16: Refactor these matches to not crash on `None`.
            // Probably add `inspect_err`, plus chain these as monadic calls.
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
                .and_then(|stream| {
                    let (sender, receiver) = channel::<TcpTunnelMessages>(1000);
                    active_connections.insert(conn.connection_id, sender);
                    task::spawn(async move { tcp_tunnel(stream.await.unwrap(), receiver).await });

                    Some(())
                });
        }
        DaemonMessage::TCPData(msg) => {
            if let Err(fail) = active_connections
                .get(&msg.connection_id)
                .map(|sender| sender.send(TcpTunnelMessages::Data(msg.data)))
                .unwrap()
                .await
            {
                debug!("sender error {:?}", fail);
                active_connections.remove(&msg.connection_id);
            }
        }
        DaemonMessage::TCPClose(msg) => {
            if let Err(fail) = active_connections
                .get(&msg.connection_id)
                .map(|sender| sender.send(TcpTunnelMessages::Close))
                .unwrap()
                .await
            {
                debug!("sender error {:?}", fail);
                active_connections.remove(&msg.connection_id);
            }
        }
        DaemonMessage::OpenFileResponse(open_file_message) => {
            debug!(
                "poll_agent -> received a FileOpen message with contents {open_file_message:?}!"
            );
            // TODO(alex) [high] 2022-05-16: After changing this part to be outside of `select!`
            // macro, we're crashing with:
            /*
                        poll_agent -> received a FileOpen message with contents FileOpenResponse { fd: 14 }!
            thread 'tokio-runtime-worker' panicked at 'called `Option::unwrap()` on a `None` value', mirrord-layer/src/lib.rs:234:22
            note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
            thread '<unnamed>' panicked at 'called `Result::unwrap()` on an `Err` value: Canceled', mirrord-layer/src/file.rs:131:51
            fatal runtime error: failed to initiate panic, error 5
                        */
            // I think this has to do with `unwrapping` outside. It only crashes for file, so that's
            // good I guess.
            unsafe {
                FILE_HOOK_SENDER
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .send(open_file_message)
            };
        }
        DaemonMessage::Close => todo!(),
        DaemonMessage::LogMessage(_) => todo!(),
    }
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<HookMessage>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut port_mapping: HashMap<Port, ListenData> = HashMap::new();
    let mut active_connections = HashMap::new();

    let open_file_handler = Mutex::new(Vec::with_capacity(4));

    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(&mut codec, &open_file_handler, &mut port_mapping, hook_message).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(&open_file_handler, &mut port_mapping, &mut active_connections, daemon_message.unwrap().unwrap()).await;
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
