#![feature(c_variadic)]
#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]
#![feature(type_alias_impl_trait)]
#![feature(generic_associated_types)]

use std::{
    env,
    lazy::{SyncLazy, SyncOnceCell},
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
    ClientCodec, ClientMessage, CloseFileRequest, CloseFileResponse, DaemonMessage, FileRequest,
    FileResponse, OpenFileRequest, OpenFileResponse, OpenRelativeFileRequest, ReadFileRequest,
    ReadFileResponse, SeekFileRequest, SeekFileResponse, WriteFileRequest, WriteFileResponse,
};
use sockets::SOCKETS;
use tcp::TcpHandler;
use tcp_mirror::TcpMirrorHandler;
use tokio::{
    runtime::Runtime,
    select,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot,
    },
    time::{sleep, Duration},
};
use tracing::{debug, error, info, trace};
use tracing_subscriber::prelude::*;

mod common;
mod config;
mod error;
mod file;
mod macros;
mod pod_api;
mod sockets;
mod tcp;
mod tcp_mirror;

use crate::{common::HookMessage, config::Config, macros::hook};

static RUNTIME: SyncLazy<Runtime> = SyncLazy::new(|| Runtime::new().unwrap());
static GUM: SyncLazy<Gum> = SyncLazy::new(|| unsafe { Gum::obtain() });

pub static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;
pub static ENABLED_FILE_OPS: SyncOnceCell<bool> = SyncOnceCell::new();

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Initializing mirrord-layer!");

    let config = Config::init_from_env().unwrap();

    let port_forwarder = RUNTIME
        .block_on(pod_api::create_agent(
            &config.impersonated_pod_name,
            &config.impersonated_pod_namespace,
            &config.agent_namespace,
            config.agent_rust_log,
            config.agent_image.unwrap_or_else(|| {
                concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
            }),
        ))
        .unwrap();

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    let enabled_file_ops = ENABLED_FILE_OPS.get_or_init(|| config.enabled_file_ops);
    enable_hooks(*enabled_file_ops);

    RUNTIME.spawn(poll_agent(port_forwarder, receiver));
}

#[allow(clippy::too_many_arguments)]
async fn handle_hook_message(
    hook_message: HookMessage,
    tcp_mirror_handler: &mut TcpMirrorHandler,
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin + Send, ClientCodec>,
    // TODO: There is probably a better abstraction for this.
    open_file_handler: &Mutex<Vec<oneshot::Sender<OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<WriteFileResponse>>>,
    close_file_handler: &Mutex<Vec<oneshot::Sender<CloseFileResponse>>>,
) {
    match hook_message {
        HookMessage::Tcp(message) => {
            tcp_mirror_handler
                .handle_hook_message(message, codec)
                .await
                .unwrap();
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

            open_file_handler.lock().unwrap().push(file_channel_tx);

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

#[allow(clippy::too_many_arguments)]
async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    tcp_mirror_handler: &mut TcpMirrorHandler,
    // TODO: There is probably a better abstraction for this.
    open_file_handler: &Mutex<Vec<oneshot::Sender<OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<WriteFileResponse>>>,
    close_file_handler: &Mutex<Vec<oneshot::Sender<CloseFileResponse>>>,
    ping: &mut bool,
) {
    match daemon_message {
        DaemonMessage::Tcp(message) => {
            tcp_mirror_handler
                .handle_daemon_message(message)
                .await
                .unwrap();
        }
        DaemonMessage::FileResponse(FileResponse::Open(open_file)) => {
            debug!("DaemonMessage::OpenFileResponse {open_file:#?}!");
            debug!("file handler = {:#?}", open_file_handler);
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
        DaemonMessage::Pong => {
            if *ping {
                *ping = false;
                trace!("Daemon sent pong!");
            } else {
                panic!("Daemon: unmatched pong!");
            }
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
    // TODO: Starting to think about a better abstraction over this whole mess. File operations are
    // pretty much just `std::fs::File` things, so I think the best approach would be to create
    // a `FakeFile`, and implement `std::io` traits on it.
    //
    // Maybe every `FakeFile` could hold it's own `oneshot` channel, read more about this on the
    // `common` module above `XHook` structs.

    // Stores a list of `oneshot`s that communicates with the hook side (send a message from -layer
    // to -agent, and when we receive a message from -agent to -layer).
    let open_file_handler = Mutex::new(Vec::with_capacity(4));
    let read_file_handler = Mutex::new(Vec::with_capacity(4));
    let seek_file_handler = Mutex::new(Vec::with_capacity(4));
    let write_file_handler = Mutex::new(Vec::with_capacity(4));
    let close_file_handler = Mutex::new(Vec::with_capacity(4));

    let mut ping = false;

    let mut tcp_mirror_handler = TcpMirrorHandler::default();

    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(),
                &mut tcp_mirror_handler,
                &mut codec,
                &open_file_handler,
                &read_file_handler,
                &seek_file_handler,
                &write_file_handler,
                &close_file_handler,
            ).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(),
                    &mut tcp_mirror_handler,
                    &open_file_handler,
                    &read_file_handler,
                    &seek_file_handler,
                    &write_file_handler,
                    &close_file_handler,
                    &mut ping,
                ).await;
            },
            _ = sleep(Duration::from_secs(60)) => {
                if !ping {
                    codec.send(ClientMessage::Ping).await.unwrap();
                    trace!("sent ping to daemon");
                    ping = true;
                } else {
                    panic!("Client: unmatched ping");
                }
            }
        }
    }
}

/// Enables file (behind `MIRRORD_FILE_OPS` option) and socket hooks.
fn enable_hooks(enabled_file_ops: bool) {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor.begin_transaction();

    hook!(interceptor, "close", close_detour);

    sockets::enable_socket_hooks(&mut interceptor);

    if enabled_file_ops {
        file::hooks::enable_file_hooks(&mut interceptor);
    }

    interceptor.end_transaction();
}

/// Attempts to close on a managed `Socket`, if there is no socket with `fd`, then this means we
/// either let the `fd` bypass and call `libc::close` directly, or it might be a managed file `fd`,
/// so it tries to do the same for files.
unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    let enabled_file_ops = ENABLED_FILE_OPS
        .get()
        .expect("Should be set during initialization!");

    if SOCKETS.lock().unwrap().remove(&fd).is_some() {
        libc::close(fd)
    } else if *enabled_file_ops {
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
