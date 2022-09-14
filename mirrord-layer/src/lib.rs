#![feature(c_variadic)]
#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]
#![feature(naked_functions)]
#![feature(result_flattening)]
#![feature(io_error_uncategorized)]
#![feature(let_chains)]
#![feature(slice_concat_trait)]

use std::{
    collections::{HashSet, VecDeque},
    sync::{LazyLock, OnceLock},
};

use common::{GetAddrInfoHook, ResponseChannel};
use ctor::ctor;
use envconfig::Envconfig;
use error::{LayerError, Result};
use file::OPEN_FILES;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use libc::c_int;
use mirrord_macro::hook_guard_fn;
use mirrord_protocol::{
    AddrInfoInternal, ClientCodec, ClientMessage, DaemonMessage, EnvVars, GetAddrInfoRequest,
    GetEnvVarsRequest,
};
use outgoing::{tcp::TcpOutgoingHandler, udp::UdpOutgoingHandler};
use rand::Rng;
use socket::SOCKETS;
use tcp::TcpHandler;
use tcp_mirror::TcpMirrorHandler;
use tcp_steal::TcpStealHandler;
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    time::{sleep, Duration},
};
use tracing::{error, info, trace};
use tracing_subscriber::prelude::*;

use crate::{
    common::HookMessage,
    config::{env::LayerEnvConfig, file::LayerFileConfig, LayerConfig},
    file::FileHandler,
};

mod common;
mod config;
mod detour;
mod error;
mod file;
mod go_env;
mod macros;
mod outgoing;
mod pod_api;
mod socket;
mod tcp;
mod tcp_mirror;
mod tcp_steal;

#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
mod go_hooks;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(detour::detour_bypass_on)
        .on_thread_stop(detour::detour_bypass_off)
        .build()
        .unwrap()
});

static GUM: LazyLock<Gum> = LazyLock::new(|| unsafe { Gum::obtain() });

pub(crate) static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;

pub(crate) static ENABLED_FILE_OPS: OnceLock<bool> = OnceLock::new();
pub(crate) static ENABLED_FILE_RO_OPS: OnceLock<bool> = OnceLock::new();
pub(crate) static ENABLED_TCP_OUTGOING: OnceLock<bool> = OnceLock::new();
pub(crate) static ENABLED_UDP_OUTGOING: OnceLock<bool> = OnceLock::new();

#[cfg(not(test))]
#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Initializing mirrord-layer!");

    let env_config = LayerEnvConfig::init_from_env().unwrap();

    let file_config = match env_config.config_file {
        Some(ref path) => LayerFileConfig::from_path(path).unwrap(),
        None => LayerFileConfig::default(),
    };

    let config = file_config.merge_with(env_config);

    let connection_port: u16 = rand::thread_rng().gen_range(30000..=65535);

    info!("Using port `{connection_port:?}` for communication");

    let port_forwarder = RUNTIME
        .block_on(pod_api::create_agent(config.clone(), connection_port))
        .unwrap_or_else(|err| match err {
            LayerError::KubeError(kube::Error::HyperError(err)) => {
                eprintln!("\nmirrord encountered an error accessing the Kubernetes API. Consider passing --accept-invalid-certificates.\n");

                match err.into_cause() {
                    Some(cause) => panic!("{}", cause),
                    None => panic!("mirrord got KubeError::HyperError"),
                }
            }
            _ => panic!("failed to create agent: {}", err),
        });

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    let enabled_file_ops =
        ENABLED_FILE_OPS.get_or_init(|| (config.enabled_file_ops || config.enabled_file_ro_ops));
    let _ = ENABLED_FILE_RO_OPS
        .get_or_init(|| (config.enabled_file_ro_ops && !config.enabled_file_ops));
    let _ = ENABLED_TCP_OUTGOING.get_or_init(|| config.enabled_tcp_outgoing);
    let _ = ENABLED_UDP_OUTGOING.get_or_init(|| config.enabled_udp_outgoing);

    enable_hooks(*enabled_file_ops, config.remote_dns);

    RUNTIME.block_on(start_layer_thread(
        port_forwarder,
        receiver,
        config,
        connection_port,
    ));
}

struct Layer<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    pub codec: actix_codec::Framed<T, ClientCodec>,
    ping: bool,
    tcp_mirror_handler: TcpMirrorHandler,
    tcp_outgoing_handler: TcpOutgoingHandler,
    udp_outgoing_handler: UdpOutgoingHandler,
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

    pub tcp_steal_handler: TcpStealHandler,

    steal: bool,
}

impl<T> Layer<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    fn new(codec: actix_codec::Framed<T, ClientCodec>, steal: bool) -> Layer<T> {
        Self {
            codec,
            ping: false,
            tcp_mirror_handler: TcpMirrorHandler::default(),
            tcp_outgoing_handler: TcpOutgoingHandler::default(),
            udp_outgoing_handler: Default::default(),
            file_handler: FileHandler::default(),
            getaddrinfo_handler_queue: VecDeque::new(),
            tcp_steal_handler: TcpStealHandler::default(),
            steal,
        }
    }

    async fn handle_hook_message(&mut self, hook_message: HookMessage) {
        trace!(
            "Layer::handle_hook_message -> hook_message {:?}",
            hook_message
        );

        match hook_message {
            HookMessage::Tcp(message) => {
                if self.steal {
                    self.tcp_steal_handler
                        .handle_hook_message(message, &mut self.codec)
                        .await
                        .unwrap();
                } else {
                    self.tcp_mirror_handler
                        .handle_hook_message(message, &mut self.codec)
                        .await
                        .unwrap();
                }
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
            HookMessage::TcpOutgoing(message) => self
                .tcp_outgoing_handler
                .handle_hook_message(message, &mut self.codec)
                .await
                .unwrap(),
            HookMessage::UdpOutgoing(message) => self
                .udp_outgoing_handler
                .handle_hook_message(message, &mut self.codec)
                .await
                .unwrap(),
        }
    }

    async fn handle_daemon_message(&mut self, daemon_message: DaemonMessage) -> Result<()> {
        match daemon_message {
            DaemonMessage::Tcp(message) => {
                self.tcp_mirror_handler.handle_daemon_message(message).await
            }
            DaemonMessage::TcpSteal(message) => {
                self.tcp_steal_handler.handle_daemon_message(message).await
            }
            DaemonMessage::File(message) => self.file_handler.handle_daemon_message(message).await,
            DaemonMessage::TcpOutgoing(message) => {
                self.tcp_outgoing_handler
                    .handle_daemon_message(message)
                    .await
            }
            DaemonMessage::UdpOutgoing(message) => {
                self.udp_outgoing_handler
                    .handle_daemon_message(message)
                    .await
            }
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
    steal: bool,
) {
    let mut layer = Layer::new(codec, steal);
    loop {
        select! {
            hook_message = receiver.recv() => {
                layer.handle_hook_message(hook_message.unwrap()).await;
            }
            Some(tcp_outgoing_message) = layer.tcp_outgoing_handler.recv() => {
                if let Err(fail) =
                    layer.codec.send(ClientMessage::TcpOutgoing(tcp_outgoing_message)).await {
                        error!("Error sending client message: {:#?}", fail);
                        break;
                    }
            }
            Some(udp_outgoing_message) = layer.udp_outgoing_handler.recv() => {
                if let Err(fail) =
                    layer.codec.send(ClientMessage::UdpOutgoing(udp_outgoing_message)).await {
                        error!("Error sending client message: {:#?}", fail);
                        break;
                    }
            }
            daemon_message = layer.codec.next() => {
                match daemon_message {
                    Some(Ok(message)) => {
                        if let Err(err) = layer.handle_daemon_message(
                            message).await {
                            error!("Error handling daemon message: {:?}", err);
                            break;
                        }
                    },
                    Some(Err(err)) => {
                        error!("Error receiving daemon message: {:?}", err);
                        break;
                    }

                    None => {
                        error!("agent disconnected");
                        break;
                    }
                }
            },
            Some(message) = layer.tcp_steal_handler.next() => {
                layer.codec.send(message).await.unwrap();
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
    connection_port: u16,
) {
    let port = pf.take_stream(connection_port).unwrap(); // TODO: Make port configurable

    // `codec` is used to retrieve messages from the daemon (messages that are sent from -agent to
    // -layer)
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());

    let (env_vars_filter, env_vars_select) = match (
        config.override_env_vars_exclude,
        config.override_env_vars_include,
    ) {
        (Some(_), Some(_)) => panic!(
            r#"mirrord-layer encountered an issue:

            mirrord doesn't support specifying both
            `OVERRIDE_ENV_VARS_EXCLUDE` and `OVERRIDE_ENV_VARS_INCLUDE` at the same time!

            > Use either `--override_env_vars_exclude` or `--override_env_vars_include`.
            >> If you want to include all, use `--override_env_vars_include="*"`."#
        ),
        (Some(exclude), None) => (HashSet::from(EnvVars(exclude)), HashSet::new()),
        (None, Some(include)) => (HashSet::new(), HashSet::from(EnvVars(include))),
        (None, None) => (HashSet::new(), HashSet::from(EnvVars("*".to_owned()))),
    };

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
            trace!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env_vars);

            for (key, value) in remote_env_vars.into_iter() {
                std::env::set_var(&key, &value);
                debug_assert_eq!(std::env::var(key), Ok(value));
            }
        } else {
            panic!("unexpected response - expected env vars response {msg:?}");
        }
    };

    let _ = tokio::spawn(thread_loop(receiver, codec, config.agent_tcp_steal_traffic));
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
    let modules = frida_gum::Module::enumerate_modules();
    let binary = &modules.first().unwrap().name;

    go_env::enable_go_env(&mut interceptor, binary);
    #[cfg(target_os = "linux")]
    #[cfg(target_arch = "x86_64")]
    {
        go_hooks::enable_socket_hooks(&mut interceptor, binary);
    }

    interceptor.end_transaction();
}

// TODO: When this is annotated with `hook_guard_fn`, then the outgoing sockets never call it (we
// just bypass). Everything works, so, should we intervene?
//
/// Attempts to close on a managed `Socket`, if there is no socket with `fd`, then this means we
/// either let the `fd` bypass and call `libc::close` directly, or it might be a managed file `fd`,
/// so it tries to do the same for files.
#[hook_guard_fn]
unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    trace!("close_detour -> fd {:#?}", fd);

    let enabled_file_ops = ENABLED_FILE_OPS
        .get()
        .expect("Should be set during initialization!");

    if SOCKETS.lock().unwrap().remove(&fd).is_some() {
        FN_CLOSE(fd)
    } else if *enabled_file_ops
        && let Some(remote_fd) = OPEN_FILES.lock().unwrap().remove(&fd) {
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
}
