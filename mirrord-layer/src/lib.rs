#![feature(c_variadic)]
#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]

use std::{
    collections::{HashSet, VecDeque},
    ops::Deref,
    sync::{LazyLock, OnceLock},
};

use common::{GetAddrInfoHook, ResponseChannel};
use ctor::ctor;
use envconfig::Envconfig;
use error::LayerError;
use file::OPEN_FILES;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use libc::c_int;
use mirrord_macro::hook_fn;
use mirrord_protocol::{
    AddrInfoInternal, ClientCodec, ClientMessage, DaemonMessage, EnvVars, GetAddrInfoRequest,
    GetEnvVarsRequest,
};
use socket::SOCKETS;
use tcp::TcpHandler;
use tcp_mirror::TcpMirrorHandler;
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
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
mod socket;
mod tcp;
mod tcp_mirror;

use crate::{common::HookMessage, config::LayerConfig, file::FileHandler};
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().unwrap());
static GUM: LazyLock<Gum> = LazyLock::new(|| unsafe { Gum::obtain() });

pub static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;

pub static ENABLED_FILE_OPS: OnceLock<bool> = OnceLock::new();

/// Wrapper around `std::sync::OnceLock`, mainly used for the `Deref` implementation to simplify
/// calls to the original functions as `FN_ORIGINAL()`, instead of `FN_ORIGINAL.get().unwrap()`.
#[derive(Debug)]
pub(crate) struct HookFn<T>(std::sync::OnceLock<T>);

impl<T> Deref for HookFn<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.get().unwrap()
    }
}

impl<T> const Default for HookFn<T> {
    fn default() -> Self {
        Self(std::sync::OnceLock::new())
    }
}

impl<T> HookFn<T> {
    pub(crate) fn set(&self, value: T) -> Result<(), T> {
        self.0.set(value)
    }
}

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Initializing mirrord-layer!");

    let config = LayerConfig::init_from_env().unwrap();

    let port_forwarder = RUNTIME
        .block_on(pod_api::create_agent(config.clone()))
        .unwrap_or_else(|e| {
            panic!("failed to create agent: {}", e);
        });

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    let enabled_file_ops = ENABLED_FILE_OPS.get_or_init(|| config.enabled_file_ops);
    enable_hooks(*enabled_file_ops, config.remote_dns);

    RUNTIME.block_on(start_layer_thread(port_forwarder, receiver, config));
}

struct Layer<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    pub codec: actix_codec::Framed<T, ClientCodec>,
    ping: bool,
    tcp_mirror_handler: TcpMirrorHandler,
    // TODO: Starting to think about a better abstraction over this whole mess. File operations are
    // pretty much just `std::fs::File` things, so I think the best approach would be to create
    // a `FakeFile`, and implement `std::io` traits on it.
    //
    // Maybe every `FakeFile` could hold it's own `oneshot` channel, read more about this on the
    // `common` module above `XHook` structs.
    file_handler: FileHandler,

    // Stores a list of `oneshot`s that communicates with the hook side (send a message from -layer
    // to -agent, and when we receive a message from -agent to -layer).
    getaddrinfo_handler_queue: VecDeque<ResponseChannel<Vec<AddrInfoInternal>>>,
}

impl<T> Layer<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    fn new(codec: actix_codec::Framed<T, ClientCodec>) -> Layer<T> {
        Self {
            codec,
            ping: false,
            tcp_mirror_handler: TcpMirrorHandler::default(),
            file_handler: FileHandler::default(),
            getaddrinfo_handler_queue: VecDeque::new(),
        }
    }

    async fn handle_hook_message(&mut self, hook_message: HookMessage) {
        match hook_message {
            HookMessage::Tcp(message) => {
                self.tcp_mirror_handler
                    .handle_hook_message(message, &mut self.codec)
                    .await
                    .unwrap();
            }
            HookMessage::File(message) => {
                self.file_handler
                    .handle_hook_message(message, &mut self.codec)
                    .await
                    .unwrap();
            }
            HookMessage::GetAddrInfoHook(GetAddrInfoHook {
                node,
                service,
                hints,
                hook_channel_tx,
            }) => {
                trace!("HookMessage::GetAddrInfo");

                self.getaddrinfo_handler_queue.push_back(hook_channel_tx);

                let request = ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest {
                    node,
                    service,
                    hints,
                });

                self.codec.send(request).await.unwrap();
            }
        }
    }

    async fn handle_daemon_message(
        &mut self,
        daemon_message: DaemonMessage,
    ) -> Result<(), LayerError> {
        match daemon_message {
            DaemonMessage::Tcp(message) => {
                self.tcp_mirror_handler.handle_daemon_message(message).await
            }
            DaemonMessage::File(message) => self.file_handler.handle_daemon_message(message).await,
            DaemonMessage::Pong => {
                if self.ping {
                    self.ping = false;
                    trace!("Daemon sent pong!");
                } else {
                    Err(LayerError::UnmatchedPong)?;
                }

                Ok(())
            }
            DaemonMessage::GetEnvVarsResponse(_) => {
                unreachable!("We get env vars only on initialization right now, shouldn't happen")
            }
            DaemonMessage::GetAddrInfoResponse(get_addr_info) => {
                trace!("DaemonMessage::GetAddrInfoResponse {:#?}", get_addr_info);

                self.getaddrinfo_handler_queue
                    .pop_front()
                    .ok_or(LayerError::SendErrorGetAddrInfoResponse)?
                    .send(get_addr_info)
                    .map_err(|_| LayerError::SendErrorGetAddrInfoResponse)
            }
            DaemonMessage::Close => todo!(),
            DaemonMessage::LogMessage(_) => todo!(),
        }
    }
}

async fn thread_loop(
    mut receiver: Receiver<HookMessage>,
    codec: actix_codec::Framed<
        impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
        ClientCodec,
    >,
) {
    let mut layer = Layer::new(codec);
    loop {
        select! {
            hook_message = receiver.recv() => {
                layer.handle_hook_message(hook_message.unwrap()).await;
            }
            daemon_message = layer.codec.next() => {
                if let Some(Ok(message)) = daemon_message {
                    if let Err(err) = layer.handle_daemon_message(
                        message).await {
                        error!("Error handling daemon message: {:?}", err);
                        break;
                    }
                } else {
                    error!("agent disconnected");
                    break;
                }
            },
            _ = sleep(Duration::from_secs(60)) => {
                if !layer.ping {
                    layer.codec.send(ClientMessage::Ping).await.unwrap();
                    trace!("sent ping to daemon");
                    layer.ping = true;
                } else {
                    panic!("Client: unmatched ping");
                }
            }
        }
    }
}

async fn start_layer_thread(
    mut pf: Portforwarder,
    receiver: Receiver<HookMessage>,
    config: LayerConfig,
) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable

    // `codec` is used to retrieve messages from the daemon (messages that are sent from -agent to
    // -layer)
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());

    if !config.override_env_vars_exclude.is_empty() && !config.override_env_vars_include.is_empty()
    {
        panic!(
            r#"mirrord-layer encountered an issue:

            mirrord doesn't support specifying both
            `OVERRIDE_ENV_VARS_EXCLUDE` and `OVERRIDE_ENV_VARS_INCLUDE` at the same time!

            > Use either `--override_env_vars_exclude` or `--override_env_vars_include`.
            >> If you want to include all, use `--override_env_vars_include="*"`."#
        );
    } else {
        let env_vars_filter = HashSet::from(EnvVars(config.override_env_vars_exclude));
        let env_vars_select = HashSet::from(EnvVars(config.override_env_vars_include));

        if !env_vars_filter.is_empty() || !env_vars_select.is_empty() {
            // TODO: Handle this error. We're just ignoring it here and letting -layer crash later.
            let _codec_result = codec
                .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                    env_vars_filter,
                    env_vars_select,
                }))
                .await;

            let msg = codec.next().await;
            if let Some(Ok(DaemonMessage::GetEnvVarsResponse(Ok(remote_env_vars)))) = msg {
                debug!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env_vars);

                for (key, value) in remote_env_vars.into_iter() {
                    debug!(
                        "DaemonMessage::GetEnvVarsResponse set key {:#?} value {:#?}",
                        key, value
                    );

                    std::env::set_var(&key, &value);
                    debug_assert_eq!(std::env::var(key), Ok(value));
                }
            } else {
                panic!("unexpected response - expected env vars response {msg:?}");
            }
        }
    }

    let _ = tokio::spawn(thread_loop(receiver, codec));
}

/// Enables file (behind `MIRRORD_FILE_OPS` option) and socket hooks.
fn enable_hooks(enabled_file_ops: bool, enabled_remote_dns: bool) {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor.begin_transaction();

    unsafe {
        let _ = replace!(&mut interceptor, "close", close_detour, FnClose, FN_CLOSE);
    };

    unsafe { socket::hooks::enable_socket_hooks(&mut interceptor, enabled_remote_dns) };

    if enabled_file_ops {
        unsafe { file::hooks::enable_file_hooks(&mut interceptor) };
    }

    interceptor.end_transaction();
}

/// Attempts to close on a managed `Socket`, if there is no socket with `fd`, then this means we
/// either let the `fd` bypass and call `libc::close` directly, or it might be a managed file `fd`,
/// so it tries to do the same for files.
#[hook_fn]
unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    let enabled_file_ops = ENABLED_FILE_OPS
        .get()
        .expect("Should be set during initialization!");

    if SOCKETS.lock().unwrap().remove(&fd).is_some() {
        FN_CLOSE(fd)
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
            FN_CLOSE(fd)
        }
    } else {
        FN_CLOSE(fd)
    }
}
