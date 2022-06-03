#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]

use std::{
    collections::HashMap,
    env,
    lazy::SyncLazy,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::Mutex,
};

use actix_codec::{AsyncRead, AsyncWrite};
use common::{
    CloseFileHook, OpenFileHook, OpenRelativeFileHook, ReadFileHook, SeekFileHook, WriteFileHook,
};
use ctor::ctor;
use envconfig::Envconfig;
use file::OPEN_FILES;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use libc::c_int;
use mirrord_protocol::{
    ClientCodec, ClientMessage, CloseFileRequest, DaemonMessage, FileRequest, FileResponse,
    OpenFileRequest, OpenRelativeFileRequest, ReadFileRequest, SeekFileRequest, WriteFileRequest,
};
use sockets::SOCKETS;
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
mod error;
mod file;
mod macros;
mod pod_api;
mod sockets;

use crate::{
    common::{HookMessage, Port},
    config::Config,
    macros::hook,
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
        config.agent_image.unwrap_or_else(|| {
            concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
        }),
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
    // TODO: There is probably a better abstraction for this.
    open_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::OpenFileResponse>>>,
    open_relative_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::WriteFileResponse>>>,
    close_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::CloseFileResponse>>>,
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
        HookMessage::OpenFileHook(OpenFileHook {
            path,
            file_channel_tx,
            open_options,
        }) => {
            debug!(
                "HookMessage::OpenFileHook path {:#?} | options {:#?}",
                path, open_options
            );

            // Lock the file handler and insert a channel that will be used to retrieve the file
            // data when it comes back from `DaemonMessage::OpenFileResponse`.
            open_file_handler.lock().unwrap().push(file_channel_tx);

            let open_file_request = OpenFileRequest { path, open_options };

            let request = ClientMessage::FileRequest(FileRequest::Open(open_file_request));
            let codec_result = codec.send(request).await;

            debug!("HookMessage::OpenFileHook codec_result {:#?}", codec_result);
        }
        HookMessage::OpenRelativeFileHook(OpenRelativeFileHook {
            relative_fd,
            path,
            file_channel_tx,
            open_options,
        }) => {
            debug!(
                "HookMessage::OpenRelativeFileHook fd {:#?} | path {:#?} | options {:#?}",
                relative_fd, path, open_options
            );

            open_relative_file_handler
                .lock()
                .unwrap()
                .push(file_channel_tx);

            let open_relative_file_request = OpenRelativeFileRequest {
                relative_fd,
                path,
                open_options,
            };

            let request =
                ClientMessage::FileRequest(FileRequest::OpenRelative(open_relative_file_request));
            let codec_result = codec.send(request).await;

            debug!("HookMessage::OpenFileHook codec_result {:#?}", codec_result);
        }
        HookMessage::ReadFileHook(ReadFileHook {
            fd,
            buffer_size,
            file_channel_tx,
        }) => {
            debug!(
                "HookMessage::ReadFileHook fd {:#?} | buffer_size {:#?}",
                fd, buffer_size
            );

            read_file_handler.lock().unwrap().push(file_channel_tx);

            debug!(
                "HookMessage::ReadFileHook read_file_handler {:#?}",
                read_file_handler
            );

            let read_file_request = ReadFileRequest { fd, buffer_size };

            debug!(
                "HookMessage::ReadFileHook read_file_request {:#?}",
                read_file_request
            );

            let request = ClientMessage::FileRequest(FileRequest::Read(read_file_request));
            let codec_result = codec.send(request).await;

            debug!("HookMessage::ReadFileHook codec_result {:#?}", codec_result);
        }
        HookMessage::SeekFileHook(SeekFileHook {
            fd,
            seek_from,
            file_channel_tx,
        }) => {
            debug!(
                "HookMessage::SeekFileHook fd {:#?} | seek_from {:#?}",
                fd, seek_from
            );

            seek_file_handler.lock().unwrap().push(file_channel_tx);

            let seek_file_request = SeekFileRequest {
                fd,
                seek_from: seek_from.into(),
            };

            let request = ClientMessage::FileRequest(FileRequest::Seek(seek_file_request));
            let codec_result = codec.send(request).await;

            debug!("HookMessage::SeekFileHook codec_result {:#?}", codec_result);
        }
        HookMessage::WriteFileHook(WriteFileHook {
            fd,
            write_bytes,
            file_channel_tx,
        }) => {
            debug!(
                "HookMessage::WriteFileHook fd {:#?} | length {:#?}",
                fd,
                write_bytes.len()
            );

            write_file_handler.lock().unwrap().push(file_channel_tx);

            let write_file_request = WriteFileRequest { fd, write_bytes };

            let request = ClientMessage::FileRequest(FileRequest::Write(write_file_request));
            let codec_result = codec.send(request).await;

            debug!(
                "HookMessage::WriteFileHook codec_result {:#?}",
                codec_result
            );
        }
        HookMessage::CloseFileHook(CloseFileHook {
            fd,
            file_channel_tx,
        }) => {
            debug!("HookMessage::CloseFileHook fd {:#?}", fd);

            close_file_handler.lock().unwrap().push(file_channel_tx);

            let close_file_request = CloseFileRequest { fd };

            let request = ClientMessage::FileRequest(FileRequest::Close(close_file_request));
            let codec_result = codec.send(request).await;

            debug!(
                "HookMessage::CloseFileHook codec_result {:#?}",
                codec_result
            );
        }
    }
}

async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    port_mapping: &mut HashMap<Port, ListenData>,
    active_connections: &mut HashMap<u16, Sender<TcpTunnelMessages>>,
    // TODO: There is probably a better abstraction for this.
    open_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::WriteFileResponse>>>,
    close_file_handler: &Mutex<Vec<oneshot::Sender<mirrord_protocol::CloseFileResponse>>>,
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
        DaemonMessage::FileResponse(FileResponse::Open(open_file)) => {
            debug!("DaemonMessage::OpenFileResponse {open_file:#?}!");

            open_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(open_file.unwrap())
                .unwrap();
        }
        DaemonMessage::FileResponse(FileResponse::Read(read_file)) => {
            // The debug message is too big if we just log it directly.
            let _ = read_file
                .as_ref()
                .inspect(|success| {
                    debug!("DaemonMessage::ReadFileResponse {:#?}", success.read_amount)
                })
                .inspect_err(|fail| error!("DaemonMessage::ReadFileResponse {:#?}", fail));

            read_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(read_file.unwrap())
                .unwrap();
        }
        DaemonMessage::FileResponse(FileResponse::Seek(seek_file)) => {
            debug!("DaemonMessage::SeekFileResponse {:#?}!", seek_file);

            seek_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(seek_file.unwrap())
                .unwrap();
        }
        DaemonMessage::FileResponse(FileResponse::Write(write_file)) => {
            debug!("DaemonMessage::WriteFileResponse {:#?}!", write_file);

            write_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(write_file.unwrap())
                .unwrap();
        }
        DaemonMessage::FileResponse(FileResponse::Close(close_file)) => {
            debug!("DaemonMessage::CloseFileResponse {:#?}!", close_file);

            close_file_handler
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .send(close_file.unwrap())
                .unwrap();
        }
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

    // TODO: Starting to think about a better abstraction over this whole mess. File operations are
    // pretty much just `std::fs::File` things, so I think the best approach would be to create
    // a `FakeFile`, and implement `std::io` traits on it.
    //
    // Maybe every `FakeFile` could hold it's own `oneshot` channel, read more about this on the
    // `common` module above `XHook` structs.

    // Stores a list of `oneshot`s that communicates with the hook side (send a message from -layer
    // to -agent, and when we receive a message from -agent to -layer).
    let open_file_handler = Mutex::new(Vec::with_capacity(4));
    let open_relative_file_handler = Mutex::new(Vec::with_capacity(4));
    let read_file_handler = Mutex::new(Vec::with_capacity(4));
    let seek_file_handler = Mutex::new(Vec::with_capacity(4));
    let write_file_handler = Mutex::new(Vec::with_capacity(4));
    let close_file_handler = Mutex::new(Vec::with_capacity(4));

    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(),
                    &mut port_mapping,
                    &mut codec,
                    &open_file_handler,
                    &open_relative_file_handler,
                    &read_file_handler,
                    &seek_file_handler,
                    &write_file_handler,
                    &close_file_handler
                ).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(),
                    &mut port_mapping,
                    &mut active_connections,
                    &open_file_handler,
                    &read_file_handler,
                    &seek_file_handler,
                    &write_file_handler,
                    &close_file_handler
                ).await;
            }
        }
    }
}

/// Enables file and socket hooks.
fn enable_hooks() {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor.begin_transaction();

    hook!(interceptor, "close", close_detour);

    sockets::enable_socket_hooks(&mut interceptor);

    if env::var("MIRRORD_FILE_OPS").is_ok() {
        file::hooks::enable_file_hooks(&mut interceptor);
    }

    interceptor.end_transaction();
}

/// Attempts to close on a managed `Socket`, if there is no socket with `fd`, then this means we
/// either let the `fd` bypass and call `libc::close` directly, or it might be a managed file `fd`,
/// so it tries to do the same for files.
unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    if SOCKETS.lock().unwrap().remove(&fd) {
        libc::close(fd)
    } else {
        if env::var("MIRRORD_FILE_OPS").is_ok() {
            let remote_fd = OPEN_FILES.lock().unwrap().remove(&fd);

            if let Some(remote_fd) = remote_fd {
                let close_file_result = file::ops::close(remote_fd);

                close_file_result
                    .map_err(|fail| {
                        error!("Failed writing file with {fail:#?}");
                        -1
                    })
                    .unwrap_or_else(|fail| fail)
            } else {
                libc::close(fd)
            }
        } else {
            libc::close(fd)
        }
    }
}
