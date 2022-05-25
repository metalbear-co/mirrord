#![feature(once_cell)]
#![feature(result_option_inspect)]

use std::{
    collections::HashMap,
    env,
    lazy::SyncLazy,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::Mutex,
};

use actix_codec::{AsyncRead, AsyncWrite};
use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use mirrord_protocol::{
    ClientCodec, ClientMessage, DaemonMessage, OpenFileRequest, ReadFileRequest, SeekFileRequest,
    WriteFileRequest,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    runtime::Runtime,
    select,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot,
    },
    task,
};
use tracing::{debug, error, info};
use tracing_subscriber::prelude::*;

mod common;
mod config;
mod file;
mod macros;
mod pod_api;
mod sockets;

use crate::{
    common::{HookMessage, Port},
    config::Config,
    sockets::{SocketInformation, CONNECTION_QUEUE},
};

static RUNTIME: SyncLazy<Runtime> = SyncLazy::new(|| Runtime::new().unwrap());
static GUM: SyncLazy<Gum> = SyncLazy::new(|| unsafe { Gum::obtain() });

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

    let port_forwarder = RUNTIME.block_on(pod_api::create_agent(
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

    RUNTIME.spawn(poll_agent(port_forwarder, receiver));
}

async fn handle_hook_message(
    hook_message: HookMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin, ClientCodec>,
    open_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::WriteFileResponse>>>,
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
        HookMessage::OpenFileHook(open) => {
            debug!("HookMessage::OpenFileHook {open:#?}");

            // NOTE(alex): Lock the file handler and insert a channel that will be used to retrieve
            // the file data when it comes back from `DaemonMessage::OpenFileResponse`.
            //
            // TODO(alex) [mid] 2022-05-19: `Vec` here may cause thread issues, @aviramha suggested
            // using a `HashMap` of request id / response id.
            open_file_handler.lock().unwrap().push(open.file_channel_tx);

            let open_file_request = OpenFileRequest {
                path: open.path,
                open_options: open.open_options,
            };

            codec
                .send(ClientMessage::OpenFileRequest(open_file_request))
                .await
                .unwrap();
        }
        HookMessage::ReadFileHook(read) => {
            debug!("HookMessage::ReadFileHook {read:#?}");

            read_file_handler.lock().unwrap().push(read.file_channel_tx);

            let read_file_request = ReadFileRequest {
                fd: read.fd,
                buffer_size: read.buffer_size,
            };

            codec
                .send(ClientMessage::ReadFileRequest(read_file_request))
                .await
                .unwrap();
        }
        HookMessage::SeekFileHook(seek) => {
            debug!("HookMessage::SeekFileHook {seek:#?}");

            seek_file_handler.lock().unwrap().push(seek.file_channel_tx);

            let seek_file_request = SeekFileRequest {
                fd: seek.fd,
                seek_from: seek.seek_from.into(),
            };

            codec
                .send(ClientMessage::SeekFileRequest(seek_file_request))
                .await
                .unwrap();
        }
        HookMessage::WriteFileHook(write) => {
            debug!("HookMessage::WriteFileHook {write:#?}");

            write_file_handler
                .lock()
                .unwrap()
                .push(write.file_channel_tx);

            let write_file_request = WriteFileRequest {
                fd: write.fd,
                write_bytes: write.write_bytes,
            };

            codec
                .send(ClientMessage::WriteFileRequest(write_file_request))
                .await
                .unwrap();
        }
    }
}

async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    active_connections: &mut HashMap<u16, Sender<TcpTunnelMessages>>,
    open_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::WriteFileResponse>>>,
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
            if let Err(fail) = active_connections
                .get(&msg.connection_id)
                .map(|sender| sender.send(TcpTunnelMessages::Data(msg.data)))
                .unwrap()
                .await
            {
                error!("DaemonMessage::TCPData error {fail:#?}");
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
                error!("DaemonMessage::TCPClose error {fail:#?}");
                active_connections.remove(&msg.connection_id);
            }
        }
        DaemonMessage::OpenFileResponse(open_file) => {
            debug!("DaemonMessage::OpenFileResponse {open_file:#?}!");

            open_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(open_file.unwrap())
                .unwrap();
        }
        DaemonMessage::ReadFileResponse(read_file) => {
            debug!("DaemonMessage::ReadFileResponse {:#?}!", read_file);

            read_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(read_file.unwrap())
                .unwrap();
        }
        DaemonMessage::SeekFileResponse(seek_file) => {
            debug!("DaemonMessage::SeekFileResponse {:#?}!", seek_file);

            seek_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(seek_file.unwrap())
                .unwrap();
        }
        DaemonMessage::WriteFileResponse(write_file) => {
            debug!("DaemonMessage::WriteFileResponse {:#?}!", write_file);

            write_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(write_file.unwrap())
                .unwrap();
        }
        DaemonMessage::Close => todo!(),
        DaemonMessage::LogMessage(_) => todo!(),
    }
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<HookMessage>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable

    // NOTE(alex): `codec` is used to retrieve messages from the daemon (messages that are sent
    // from -agent to -layer)
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut port_mapping: HashMap<Port, ListenData> = HashMap::new();
    let mut active_connections = HashMap::new();

    // TODO(alex) [low] 2022-05-22: Starting to think about a better abstraction over this whole
    // mess. File operations are pretty much just `std::fs::File` things, so I think the best
    // approach would be to create a `FakeFile`, and implement `std::io` traits on it.
    //
    // Maybe every `FakeFile` could hold it's own `oneshot` channel, read more about this on the
    // `common` module above `XHook` structs.
    //
    // NOTE(alex): Stores a list of `oneshot`s that notifies (and retrieves data) the `open` hook
    // when -layer receives a `DaemonMessage::OpenFileResponse`.
    let open_file_handler = Mutex::new(Vec::with_capacity(4));
    let read_file_handler = Mutex::new(Vec::with_capacity(4));
    let seek_file_handler = Mutex::new(Vec::with_capacity(4));
    let write_file_handler = Mutex::new(Vec::with_capacity(4));

    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(),
                    &mut port_mapping,
                    &mut codec,
                    &open_file_handler,
                    &read_file_handler,
                    &seek_file_handler,
                    &write_file_handler
                ).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(),
                    &mut port_mapping,
                    &mut active_connections,
                    &open_file_handler,
                    &read_file_handler,
                    &seek_file_handler,
                    &write_file_handler
                ).await;
            }
        }
    }
}

/// Enables file and socket hooks.
fn enable_hooks() {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor.begin_transaction();

    sockets::enable_socket_hooks(&mut interceptor);

    if env::var("MIRROD_FILE_OPS").is_ok() {
        file::hooks::enable_file_hooks(&mut interceptor);
    }

    interceptor.end_transaction();
}
