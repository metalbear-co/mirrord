#![feature(c_variadic)]
#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(const_trait_impl)]
#![feature(naked_functions)]
#![feature(result_flattening)]
#![feature(io_error_uncategorized)]
#![feature(let_chains)]
#![feature(async_closure)]
#![feature(try_trait_v2)]
#![feature(try_trait_v2_residual)]
#![feature(trait_alias)]

extern crate alloc;
use std::{
    collections::{HashSet, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::{LazyLock, OnceLock},
};

use actix_codec::{AsyncRead, AsyncWrite};
use common::{GetAddrInfoHook, ResponseChannel};
use ctor::ctor;
use error::{LayerError, Result};
use file::{filter::FileFilter, OPEN_FILES};
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use libc::c_int;
use mirrord_config::{config::MirrordConfig, util::VecOrSingle, LayerConfig, LayerFileConfig};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest},
    ClientCodec, ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest,
};
use outgoing::{tcp::TcpOutgoingHandler, udp::UdpOutgoingHandler};
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
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    common::HookMessage,
    file::{filter::FILE_FILTER, FileHandler},
};

mod common;
mod connection;
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
mod tracing_util;

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

/// Check if we're running in NixOS or Devbox
/// if so, add `sh` to the skip list because of https://github.com/metalbear-co/mirrord/issues/531
fn nix_devbox_patch(config: &mut LayerConfig) {
    let mut current_skip = config
        .skip_processes
        .clone()
        .map(VecOrSingle::to_vec)
        .unwrap_or_default();

    if is_nix_or_devbox() && !current_skip.contains(&"sh".to_string()) {
        current_skip.push("sh".into());
        std::env::set_var("MIRRORD_SKIP_PROCESSES", current_skip.join(";"));
        config.skip_processes = Some(VecOrSingle::Multiple(current_skip));
    }
}

/// Check if NixOS or Devbox by discrimnating env vars.
fn is_nix_or_devbox() -> bool {
    if let Ok(res) = std::env::var("IN_NIX_SHELL") && res.as_str() == "1" {
        true
    }
    else if let Ok(res) = std::env::var("DEVBOX_SHELL_ENABLED") && res.as_str() == "1" {
        true
    } else {
        false
    }
}

/// Prevent mirrord from connecting to ports used by the intelliJ debugger
pub(crate) fn port_debug_patch(addr: SocketAddr) -> bool {
    if let Ok(ports) = std::env::var("DEBUGGER_IGNORE_PORTS_PATCH") {
        let (ip, port) = (addr.ip(), addr.port());
        let ignored_ip =
            ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || ip == IpAddr::V6(Ipv6Addr::LOCALHOST);
        // port range can be specified as "45000-65000" or just "45893"
        let ports: Vec<u16> = ports
            .split('-')
            .map(|p| {
                p.parse()
                    .expect("Failed to parse the given port - not a number!")
            })
            .collect();
        match ports.len() {
            2 => ignored_ip && (port >= ports[0] && port <= ports[1]),
            1 => ignored_ip && port == ports[0],
            _ => false,
        }
    } else {
        false
    }
}

/// Loads mirrord configuration and applies [`nix_devbox_patch`] patches.
fn layer_pre_initialization() -> Result<(), LayerError> {
    let args = std::env::args().collect::<Vec<_>>();
    let given_process = args
        .first()
        .and_then(|arg| arg.split('/').last())
        .ok_or(LayerError::NoProcessFound)?;

    let mut config = std::env::var("MIRRORD_CONFIG_FILE")
        .map(PathBuf::from)
        .map(|path| LayerFileConfig::from_path(&path))?
        .unwrap_or_default()
        .generate_config()?;

    nix_devbox_patch(&mut config);
    let skip_processes = config.skip_processes.clone().map(VecOrSingle::to_vec);

    if should_load(given_process, skip_processes) {
        layer_start(config);
    }

    Ok(())
}

/// The one true start of mirrord-layer.
#[cfg(not(test))]
#[ctor]
fn mirrord_layer_entry_point() {
    if let Err(fail) = layer_pre_initialization() {
        // TODO(alex) [mid] 2022-11-08: Improve these.
        eprintln!("Failed initialization with {fail:#?}");
        panic!("mirrord-layer: Cannot recover, closing!")
    }
}

/// Occurs after [`layer_pre_initialization`] has succeeded.
///
/// Starts the main parts of mirrord-layer.
fn layer_start(config: LayerConfig) {
    if config.feature.capture_error_trace {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(tracing_util::file_tracing_writer())
                    .with_ansi(false)
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::ACTIVE),
            )
            .with(tracing_subscriber::EnvFilter::new("mirrord=trace"))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::ACTIVE)
                    .compact(),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    };

    info!("Initializing mirrord-layer!");

    let connection = RUNTIME.block_on(connection::connect(&config));

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    let enabled_file_ops = ENABLED_FILE_OPS
        .get_or_init(|| config.feature.fs.is_read() || config.feature.fs.is_write());
    ENABLED_FILE_RO_OPS
        .set(config.feature.fs.is_read())
        .expect("Setting ENABLED_FILE_RO_OPS singleton");
    ENABLED_TCP_OUTGOING
        .set(config.feature.network.outgoing.tcp)
        .expect("Setting ENABLED_TCP_OUTGOING singleton");
    ENABLED_UDP_OUTGOING
        .set(config.feature.network.outgoing.udp)
        .expect("Setting ENABLED_UDP_OUTGOING singleton");

    FILE_FILTER.get_or_init(|| FileFilter::new(config.feature.fs.clone()));

    enable_hooks(*enabled_file_ops, config.feature.network.dns);

    RUNTIME.block_on(start_layer_thread(connection, receiver, config));
}

fn should_load(given_process: &str, skip_processes: Option<Vec<String>>) -> bool {
    if let Some(processes_to_avoid) = skip_processes {
        !processes_to_avoid.iter().any(|x| x == given_process)
    } else {
        true
    }
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
    getaddrinfo_handler_queue: VecDeque<ResponseChannel<DnsLookup>>,

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

    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_hook_message(&mut self, hook_message: HookMessage) {
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
                hook_channel_tx,
            }) => {
                self.getaddrinfo_handler_queue.push_back(hook_channel_tx);
                let request = ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node });

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

    #[tracing::instrument(level = "trace", skip(self))]
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
            DaemonMessage::GetAddrInfoResponse(get_addr_info) => self
                .getaddrinfo_handler_queue
                .pop_front()
                .ok_or(LayerError::SendErrorGetAddrInfoResponse)?
                .send(get_addr_info.0)
                .map_err(|_| LayerError::SendErrorGetAddrInfoResponse),
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
    config: LayerConfig,
) {
    let mut layer = Layer::new(codec, config.feature.network.incoming.is_steal());
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
                            if let LayerError::SendErrorConnection(_) = err {
                                info!("Connection closed by agent");
                                continue;
                            }
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

    if config.feature.capture_error_trace {
        tracing_util::print_support_message();
    }
    graceful_exit!("mirrord has encountered an error and is now exiting.");
}

#[tracing::instrument(level = "trace", skip(connection, receiver))]
async fn start_layer_thread(
    connection: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    receiver: Receiver<HookMessage>,
    config: LayerConfig,
) {
    // `codec` is used to retrieve messages from the daemon (messages that are sent from -agent to
    // -layer)
    let mut codec = actix_codec::Framed::new(connection, ClientCodec::new());

    let (env_vars_filter, env_vars_select) = match (
        config
            .feature
            .env
            .exclude
            .clone()
            .map(|exclude| exclude.join(";")),
        config
            .feature
            .env
            .include
            .clone()
            .map(|include| include.join(";")),
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

        select! {
          msg = codec.next() => {
            if let Some(Ok(DaemonMessage::GetEnvVarsResponse(Ok(remote_env_vars)))) = msg {
                trace!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env_vars);

                for (key, value) in remote_env_vars.into_iter() {
                    std::env::set_var(&key, &value);
                    debug_assert_eq!(std::env::var(key), Ok(value));
                }

                if let Some(overrides) = &config.feature.env.overrides {
                    for (key, value) in overrides {
                        std::env::set_var(key, value);
                    }
                }
            } else {
                let raw_issue = format!("Expected env vars response, but got {msg:?}");
                graceful_exit!("{}{FAIL_STILL_STUCK}{}", FAIL_UNEXPECTED_RESPONSE, raw_issue);
            }
          },
          _ = sleep(Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into())) => {
            graceful_exit!(r#"
                agent response timeout - expected env var response

                check that the agent image can run on your architecture
            "#);
          }
        }
    };

    let _ = tokio::spawn(thread_loop(receiver, codec, config));
}

/// Enables file (behind `MIRRORD_FILE_OPS` option) and socket hooks.
#[tracing::instrument(level = "trace")]
fn enable_hooks(enabled_file_ops: bool, enabled_remote_dns: bool) {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor.begin_transaction();

    unsafe {
        let _ = replace!(&mut interceptor, "close", close_detour, FnClose, FN_CLOSE);
        let _ = replace!(
            &mut interceptor,
            "close$NOCANCEL",
            close_nocancel_detour,
            FnClose_nocancel,
            FN_CLOSE_NOCANCEL
        );
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
        go_hooks::enable_hooks(&mut interceptor, binary);
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
pub(crate) unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
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
                error!("Failed closing file with {fail:#?}");
                -1
            })
            .unwrap_or_else(|fail| fail)
    } else {
        FN_CLOSE(fd)
    }
}

// no need to guard because we call another detour which will do the guard for us.
#[hook_fn]
pub(crate) unsafe extern "C" fn close_nocancel_detour(fd: c_int) -> c_int {
    close_detour(fd)
}

pub(crate) const FAIL_STILL_STUCK: &str = r#"
- If you're still stuck and everything looks fine:

>> Please open a new bug report at https://github.com/metalbear-co/mirrord/issues/new/choose

>> Or join our discord https://discord.com/invite/J5YSrStDKD and request help in #mirrord-help.

"#;

const FAIL_UNEXPECTED_RESPONSE: &str = r#"
mirrord-layer received an unexpected response from the agent pod!

- Suggestions:

>> When trying to run a program with arguments in the form of `app -arg value`, run it as
   `app -- -arg value` instead.
"#;

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("test", Some(vec!["foo".to_string()]))]
    #[case("test", None)]
    #[case("test", Some(vec!["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]))]
    fn test_should_load_true(
        #[case] given_process: &str,
        #[case] skip_processes: Option<Vec<String>>,
    ) {
        assert!(should_load(given_process, skip_processes));
    }

    #[rstest]
    #[case("test", Some(vec!["test".to_string()]))]
    #[case("test", Some(vec!["test".to_owned(), "foo".to_owned(), "bar".to_owned(), "baz".to_owned()]))]
    fn test_should_load_false(
        #[case] given_process: &str,
        #[case] skip_processes: Option<Vec<String>>,
    ) {
        assert!(!should_load(given_process, skip_processes));
    }
}
