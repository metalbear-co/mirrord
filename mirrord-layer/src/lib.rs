#![feature(c_variadic)]
#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]

use std::{
    collections::HashSet,
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
    ClientCodec, ClientMessage, CloseFileRequest, CloseFileResponse, DaemonMessage, EnvVarsFilter,
    FileRequest, FileResponse, GetEnvVarsRequest, OpenFileRequest, OpenFileResponse,
    OpenRelativeFileRequest, ReadFileRequest, ReadFileResponse, SeekFileRequest, SeekFileResponse,
    WriteFileRequest, WriteFileResponse,
};
use sockets::SOCKETS;
use tcp_mirror::create_tcp_mirror_handler;
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

use crate::{common::HookMessage, config::LayerConfig, macros::hook, tcp::TCPApi};

static RUNTIME: SyncLazy<Runtime> = SyncLazy::new(|| Runtime::new().unwrap());
static GUM: SyncLazy<Gum> = SyncLazy::new(|| unsafe { Gum::obtain() });

pub static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;
pub static ENABLED_FILE_OPS: SyncOnceCell<bool> = SyncOnceCell::new();
pub static ENABLED_OVERRIDE_ENV_VARS: SyncOnceCell<bool> = SyncOnceCell::new();

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Initializing mirrord-layer!");

    let config = LayerConfig::init_from_env().unwrap();

    let port_forwarder = RUNTIME.block_on(pod_api::create_agent(config.clone()));

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    let enabled_file_ops = ENABLED_FILE_OPS.get_or_init(|| config.enabled_file_ops);
    enable_hooks(*enabled_file_ops);

    RUNTIME.spawn(poll_agent(port_forwarder, receiver, config));
}

#[allow(clippy::too_many_arguments)]
async fn handle_hook_message(
    hook_message: HookMessage,
    mirror_api: &mut TCPApi,
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin, ClientCodec>,
    // TODO: There is probably a better abstraction for this.
    open_file_handler: &Mutex<Vec<oneshot::Sender<OpenFileResponse>>>,
    open_relative_file_handler: &Mutex<Vec<oneshot::Sender<OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<WriteFileResponse>>>,
    close_file_handler: &Mutex<Vec<oneshot::Sender<CloseFileResponse>>>,
) {
    match hook_message {
        HookMessage::Listen(listen_message) => {
            debug!("HookMessage::Listen {:?}", listen_message);
            codec
                .send(ClientMessage::PortSubscribe(vec![listen_message.real_port]))
                .await
                .map(|()| async { mirror_api.listen_request(listen_message).await.unwrap() })
                .unwrap()
                .await;
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

#[allow(clippy::too_many_arguments)]
async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    mirror_api: &mut TCPApi,
    // TODO: There is probably a better abstraction for this.
    open_file_handler: &Mutex<Vec<oneshot::Sender<OpenFileResponse>>>,
    read_file_handler: &Mutex<Vec<oneshot::Sender<ReadFileResponse>>>,
    seek_file_handler: &Mutex<Vec<oneshot::Sender<SeekFileResponse>>>,
    write_file_handler: &Mutex<Vec<oneshot::Sender<WriteFileResponse>>>,
    close_file_handler: &Mutex<Vec<oneshot::Sender<CloseFileResponse>>>,
    ping: &mut bool,
) {
    match daemon_message {
        DaemonMessage::NewTCPConnection(tcp_connection) => {
            debug!("DaemonMessage::NewTCPConnection {:#?}", tcp_connection);
            mirror_api.new_tcp_connection(tcp_connection).await.unwrap();
        }
        DaemonMessage::TCPData(tcp_data) => {
            debug!(
                "DaemonMessage::TCPData id {:#?} | amount {:#?}",
                tcp_data.connection_id,
                tcp_data.bytes.len()
            );
            mirror_api.tcp_data(tcp_data).await.unwrap();
        }
        DaemonMessage::TCPClose(tcp_close) => {
            debug!("DaemonMessage::TCPClose {:#?}", tcp_close);
            mirror_api.tcp_close(tcp_close).await.unwrap();
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
        DaemonMessage::Pong => {
            if *ping {
                *ping = false;
                trace!("Daemon sent pong!");
            } else {
                panic!("Daemon: unmatched pong!");
            }
        }
        DaemonMessage::GetEnvVarsResponse(remote_env_vars) => {
            debug!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env_vars);

            match remote_env_vars {
                Ok(remote_env_vars) => {
                    for (key, value) in remote_env_vars.into_iter() {
                        debug!(
                            "DaemonMessage::GetEnvVarsResponse set key {:#?} value {:#?}",
                            key, value
                        );

                        std::env::set_var(&key, &value);
                        debug_assert_eq!(std::env::var(key), Ok(value));
                    }
                }
                Err(fail) => error!(
                    "Loading remote environment variables failed with {:#?}",
                    fail
                ),
            }
        }
        DaemonMessage::Close => todo!(),
        DaemonMessage::LogMessage(_) => todo!(),
    }
}

async fn poll_agent(
    mut pf: Portforwarder,
    mut receiver: Receiver<HookMessage>,
    config: LayerConfig,
) {
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
    let open_relative_file_handler = Mutex::new(Vec::with_capacity(4));
    let read_file_handler = Mutex::new(Vec::with_capacity(4));
    let seek_file_handler = Mutex::new(Vec::with_capacity(4));
    let write_file_handler = Mutex::new(Vec::with_capacity(4));
    let close_file_handler = Mutex::new(Vec::with_capacity(4));

    let mut ping = false;

    if config.enabled_override_env_vars {
        let env_vars_filter = HashSet::from(EnvVarsFilter(config.override_env_vars_filter));

        let codec_result = codec
            .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
            }))
            .await;

        debug!(
            "ClientMessage::GetEnvVarsFilterRequest codec_result {:#?}",
            codec_result
        );
    }

    let (tcp_mirror_handler, mut mirror_api, handler_receiver) = create_tcp_mirror_handler();
    tokio::spawn(async move { tcp_mirror_handler.run(handler_receiver).await });

    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(),
                &mut mirror_api,
                &mut codec,
                &open_file_handler,
                &open_relative_file_handler,
                &read_file_handler,
                &seek_file_handler,
                &write_file_handler,
                &close_file_handler,
            ).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(),
                    &mut mirror_api,
                    &open_file_handler,
                    &read_file_handler,
                    &seek_file_handler,
                    &write_file_handler,
                    &close_file_handler,
                    &mut ping,
                ).await;
            }
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
