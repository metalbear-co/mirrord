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
#![feature(c_size_t)]
#![feature(pointer_byte_offsets)]
#![allow(rustdoc::private_intra_doc_links)]

//! Loaded dynamically with your local process.
//!
//! Paired with [`mirrord-agent`], it makes your local process behave as if it was running in a
//! remote context.
//!
//! Check out the [Introduction](https://mirrord.dev/docs/overview/introduction/) guide to learn
//! more about mirrord.
//!
//! ## How it works
//!
//! This crate intercepts your processes' [`libc`] calls, and instead of executing them locally (as
//! normal), it instead forwards them as a message to the mirrord-agent pod.
//! The operation is executed there, with the result being returned back to `mirrord-layer`, and
//! finally to the original [`libc`] call.
//!
//! ### Example
//!
//! Let's say you have a Node.js app that just opens a file, like this:
//!
//! - `open-file.mjs`
//!
//! ```js
//! import { open } from 'node:fs';
//!
//! const file = open('/tmp/hello.txt');
//! ```
//!
//! When run with mirrord, this is what's going to happen:
//!
//! 1. We intercept the call to [`libc::open`] using our `open_detour` hook, which calls
//!  [`file::ops::open`];
//!
//! 2. [`file::ops::open`] sends an open file message to `mirrord-agent`;
//!
//! 3. `mirrore-agent` tries to open `/tmp/hello.txt` in the remote context it's running, and
//! returns the result of the operation back to `mirrord-layer`;
//!
//! 4. We handle the mapping of the remote file (the one we have open in `mirrord-agent`), and a
//! local file (temporarily created);
//!
//! 5. And finally, we return the expected result (type) to your Node.js application, as if it had
//! just called [`libc::open`].
//!
//! Your application will get an fd that is valid in the context of mirrord, and calls to other file
//! functions (like [`libc::read`]), will work just fine, operating on the remote file.
//!
//! ## Configuration
//!
//! The functions we intercept are controlled via the `mirrord-config` crate, check its
//! documentation for more details, or
//! [Configuration](https://mirrord.dev/docs/overview/configuration/) for usage information.

extern crate alloc;
extern crate core;

use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    panic,
    sync::{LazyLock, OnceLock},
};

use common::ResponseChannel;
use ctor::ctor;
use dns::GetAddrInfo;
use error::{LayerError, Result};
use file::{filter::FileFilter, OPEN_FILES};
use hooks::HookManager;
use libc::c_int;
use mirrord_config::{
    feature::FeatureConfig,
    fs::FsConfig,
    incoming::{http_filter::HttpHeaderFilterConfig, IncomingConfig},
    network::NetworkConfig,
    util::VecOrSingle,
    LayerConfig,
};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest},
    tcp::{HttpResponse, LayerTcpSteal},
    ClientMessage, DaemonMessage,
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
    load::LoadType,
    socket::CONNECTION_QUEUE,
};

mod common;
mod connection;
mod detour;
mod dns;
mod error;
#[cfg(target_os = "macos")]
mod exec;
mod file;
mod hooks;
mod load;
mod macros;
mod outgoing;
mod socket;
mod tcp;
mod tcp_mirror;
mod tcp_steal;
mod tracing_util;

#[cfg(target_os = "linux")]
#[cfg(target_arch = "x86_64")]
mod go_hooks;

/// Main tokio [`Runtime`] for mirrord-layer async tasks.
///
/// Apart from some pre-initialization steps, mirrord-layer mostly runs inside this runtime with
/// `RUNTIME.block_on`.
///
/// ## Usage
///
/// Currently it's being used to run 2 big tasks:
///
/// 1. [`connection::connect`]: which creates the mirrord-agent connection for this layer instance;
///
/// 2. [`start_layer_thread`]: where we [`spawn`](tokio::spawn) mirrord-layer's main loop.
///
/// ## Bypass
///
/// To prevent us from intercepting neccessary (local) syscalls (like creating a socket), we use
/// `detour::detour_bypass_on`] [`on_thread_start`, and [`detour::detour_bypass_off`]
/// `on_thread_stop`.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(detour::detour_bypass_on)
        .on_thread_stop(detour::detour_bypass_off)
        .build()
        .unwrap()
});

/// [`Sender`] for the [`HookMessage`]s that are handled internally, and converted (when applicable)
/// to [`ClientMessage`]s.
///
/// The messages sent through here originate from the hooks, and are (usually) converted
/// to [`ClientMessage`]s.
///
/// ## Usage
///
/// You probably don't want to use this directly, instead prefer calling
/// [`blocking_send_hook_message`](common::blocking_send_hook_message) to send internal messages.
pub(crate) static HOOK_SENDER: OnceLock<Sender<HookMessage>> = OnceLock::new();

/// Holds the file operations configuration, as specified by [`FsConfig`].
///
/// ## Usage
///
/// Mainly used to share the [`FsConfig`] in places where we can't easily pass this as an argument
/// to some function:
///
/// 1. [`close_layer_fd`];
/// 2. [`go_hooks`] file operations.
pub(crate) static FILE_MODE: OnceLock<FsConfig> = OnceLock::new();

/// Tells us if the user enabled the Tcp outgoing feature in [`OutgoingConfig`].
///
/// ## Usage
///
/// Used to change the behavior of the `socket::ops::connect` hook operation.
pub(crate) static ENABLED_TCP_OUTGOING: OnceLock<bool> = OnceLock::new();

/// Tells us if the user enabled the Udp outgoing feature in [`OutgoingConfig`].
///
/// ## Usage
///
/// Used to change the behavior of the `socket::ops::connect` hook operation.
pub(crate) static ENABLED_UDP_OUTGOING: OnceLock<bool> = OnceLock::new();

/// Tells us if the user enabled wants to ignore localhots connections in [`OutgoingConfig`].
///
/// ## Usage
///
/// When true, localhost connections will stay local (won't go to the remote pod localhost)
pub(crate) static OUTGOING_IGNORE_LOCALHOST: OnceLock<bool> = OnceLock::new();

/// Tells us if the user enabled wants to ignore listening on localhost in [`IncomingConfig`].
///
/// ## Usage
///
/// When true, localhost connections will stay local - wont mirror or steal.
pub(crate) static INCOMING_IGNORE_LOCALHOST: OnceLock<bool> = OnceLock::new();

/// Check if we're running in NixOS or Devbox.
///
/// - If so, add `sh` to the skip list because of
/// [#531](https://github.com/metalbear-co/mirrord/issues/531)
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
pub(crate) fn port_debug_patch(addr: &SocketAddr) -> bool {
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

    let mut config = LayerConfig::from_env()?;

    nix_devbox_patch(&mut config);

    #[cfg(target_os = "macos")]
    let patch_binaries = config
        .sip_binaries
        .clone()
        .map(|x| x.to_vec())
        .unwrap_or_default();

    match load::load_type(given_process, config) {
        LoadType::Full(config) => layer_start(*config),
        #[cfg(target_os = "macos")]
        LoadType::SIPOnly => sip_only_layer_start(patch_binaries),
        LoadType::Skip => {}
    }

    Ok(())
}

/// The one true start of mirrord-layer.
///
/// Calls [`layer_pre_initialization`], which runs mirrord-layer.
#[ctor]
fn mirrord_layer_entry_point() {
    // If we try to use `#[cfg(not(test))]`, it gives a bunch of unused warnings, unless you specify
    // a profile, for example `cargo check --profile=dev`.
    if !cfg!(test) {
        let _ = panic::catch_unwind(|| {
            if let Err(fail) = layer_pre_initialization() {
                match fail {
                    LayerError::NoProcessFound => (),
                    _ => {
                        eprintln!("mirrord layer setup failed with {fail:?}");
                        std::process::exit(-1)
                    }
                }
            }
        });
    }
}

/// Occurs after [`layer_pre_initialization`] has succeeded.
///
/// Starts the main parts of mirrord-layer.
///
/// ## Details
///
/// Sets up a few things based on the [`LayerConfig`] given by the user:
///
/// 1. [`tracing_subscriber`], or [`mirrord_console`];
///
/// 2. Connects to the mirrord-agent with [`connection::connect`];
///
/// 3. Initializes some of our globals;
///
/// 4. Replaces the [`libc`] calls with our hooks with [`enable_hooks`];
///
/// 5. Starts the main mirrord-layer thread.
fn layer_start(config: LayerConfig) {
    if config.feature.capture_error_trace {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(tracing_util::file_tracing_writer())
                    .with_ansi(false)
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE),
            )
            .with(tracing_subscriber::EnvFilter::new("mirrord=trace"))
            .init();
    } else if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        RUNTIME
            .block_on(mirrord_console::init_logger(&console_addr))
            .expect("logger initialization failed");
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .compact()
                    .with_writer(std::io::stderr),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    };

    info!("Initializing mirrord-layer!");
    let exe_path = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    trace!(
        "Loaded into executable: {}, with args: {:?}",
        &exe_path,
        std::env::args()
    );

    let address = config
        .connect_tcp
        .as_ref()
        .expect("layer loaded without proxy address to connect to")
        .parse()
        .expect("couldn't parse proxy address");

    let (tx, rx) = RUNTIME.block_on(connection::connect_to_proxy(address));

    let (sender, receiver) = channel::<HookMessage>(1000);
    HOOK_SENDER
        .set(sender)
        .expect("Setting HOOK_SENDER singleton");

    let file_mode = FILE_MODE.get_or_init(|| config.feature.fs.clone());
    ENABLED_TCP_OUTGOING
        .set(config.feature.network.outgoing.tcp)
        .expect("Setting ENABLED_TCP_OUTGOING singleton");
    ENABLED_UDP_OUTGOING
        .set(config.feature.network.outgoing.udp)
        .expect("Setting ENABLED_UDP_OUTGOING singleton");

    OUTGOING_IGNORE_LOCALHOST
        .set(config.feature.network.outgoing.ignore_localhost)
        .expect("Setting OUTGOING_IGNORE_LOCALHOST singleton");

    INCOMING_IGNORE_LOCALHOST
        .set(config.feature.network.incoming.ignore_localhost)
        .expect("Setting INCOMING_IGNORE_LOCALHOST singleton");

    FILE_FILTER.get_or_init(|| FileFilter::new(config.feature.fs.clone()));

    enable_hooks(
        file_mode.is_active(),
        config.feature.network.dns,
        config
            .sip_binaries
            .clone()
            .map(|x| x.to_vec())
            .unwrap_or_default(),
    );

    RUNTIME.block_on(start_layer_thread(tx, rx, receiver, config));
}

/// We need to hook execve syscall to allow mirrord-layer to be loaded with sip patch when loading
/// mirrord-layer on a process where specified to skip with MIRRORD_SKIP_PROCESSES
#[cfg(target_os = "macos")]
fn sip_only_layer_start(patch_binaries: Vec<String>) {
    let mut hook_manager = HookManager::default();

    unsafe { exec::enable_execve_hook(&mut hook_manager, patch_binaries) };
}

/// Acts as an API to the various features of mirrord-layer, holding the actual feature handler
/// types.
struct Layer {
    /// Used by the many `T::handle_hook_message` functions to send [`ClientMessage`]s to
    /// mirrord-agent.
    ///
    /// The [`Receiver`] lives in the
    /// [`wrap_raw_connection`](mirrord_kube::api::wrap_raw_connection) loop, where we read from
    /// this channel and send the messages to the agent.
    tx: Sender<ClientMessage>,

    /// Used in the [`thread_loop`] to read [`DaemonMessage`]s and pass them to
    /// `handle_daemon_message`.
    ///
    /// The [`Sender`] lives in the [`wrap_raw_connection`](mirrord_kube::api::wrap_raw_connection)
    /// loop, where we receive the remote [`DaemonMessage`]s, and send them through it.
    rx: Receiver<DaemonMessage>,

    /// Part of the heartbeat mechanism of mirrord.
    ///
    /// When it's been too long since we've last received a message (60 seconds), and this is
    /// `false`, we send a [`ClientMessage::Ping`], set this to `true`, and expect to receive a
    /// [`DaemonMessage::Pong`], setting it to `false` and completing the hearbeat detection.
    ping: bool,

    /// Handler to the TCP mirror operations, see [`TcpMirrorHandler`].
    tcp_mirror_handler: TcpMirrorHandler,

    /// Handler to the TCP outgoing operations, see [`TcpOutgoingHandler`].
    tcp_outgoing_handler: TcpOutgoingHandler,

    /// Handler to the UDP outgoing operations, see [`UdpOutgoingHandler`].
    udp_outgoing_handler: UdpOutgoingHandler,
    // TODO: Starting to think about a better abstraction over this whole mess. File operations are
    // pretty much just `std::fs::File` things, so I think the best approach would be to create
    // a `FakeFile`, and implement `std::io` traits on it.
    //
    // Maybe every `FakeFile` could hold it's own `oneshot` channel, read more about this on the
    // `common` module above `XHook` structs.
    file_handler: FileHandler,

    /// Handles the DNS lookup response we get from the agent, see
    /// `getaddrinfo`.
    ///
    /// Unlike most of the other [`DaemonMessage`] handlers, this message is handled directly in
    /// `handle_daemon_message`.
    ///
    /// These are a list of [`oneshot::Sender`](tokio::sync::oneshot::Sender) channels containing
    /// the [`RemoteResult`](mirrord_protocol::codec::RemoteResult)s of the [`DnsLookup`] operation
    /// from the agent.
    getaddrinfo_handler_queue: VecDeque<ResponseChannel<DnsLookup>>,

    /// Handler to the TCP stealer operations, see [`UdpOutgoingHandler`].
    pub tcp_steal_handler: TcpStealHandler,

    /// Receives responses in the layer loop to be forwarded to the agent.
    ///
    /// Part of the stealer HTTP traffic feature, see `http`.
    pub http_response_receiver: Receiver<HttpResponse>,

    /// Sets the way we handle [`HookMessage::Tcp`] in `handle_hook_message`:
    ///
    /// - `true`: uses `tcp_steal_handler`;
    /// - `false`: uses `tcp_mirror_handler`.
    steal: bool,
}

impl Layer {
    fn new(
        tx: Sender<ClientMessage>,
        rx: Receiver<DaemonMessage>,
        incoming: IncomingConfig,
    ) -> Layer {
        // TODO: buffer size?
        let (http_response_sender, http_response_receiver) = channel(1024);
        let steal = incoming.is_steal();
        let IncomingConfig {
            http_header_filter: HttpHeaderFilterConfig { filter, ports },
            port_mapping,
            ..
        } = incoming;
        Self {
            tx,
            rx,
            ping: false,
            tcp_mirror_handler: TcpMirrorHandler::new(port_mapping.clone()),
            tcp_outgoing_handler: TcpOutgoingHandler::default(),
            udp_outgoing_handler: Default::default(),
            file_handler: FileHandler::default(),
            getaddrinfo_handler_queue: VecDeque::new(),
            tcp_steal_handler: TcpStealHandler::new(
                filter,
                ports.into(),
                http_response_sender,
                port_mapping,
            ),
            http_response_receiver,
            steal,
        }
    }

    /// Sends a [`ClientMessage`] through `Layer::tx` to the [`Receiver`] in
    /// [`wrap_raw_connection`](mirrord_kube::api::wrap_raw_connection).
    async fn send(&self, msg: ClientMessage) -> Result<(), ClientMessage> {
        self.tx.send(msg).await.map_err(|err| err.0)
    }

    /// Passes most of the [`HookMessage`]s to their appropriate handlers, together with the
    /// `Layer::tx` channel.
    ///
    /// ## Special case
    ///
    /// The [`HookMessage::GetAddrInfo`] message is dealt with here, we convert it to a
    /// [`ClientMessage::GetAddrInfoRequest`], and send it with [`Self::send`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_hook_message(&mut self, hook_message: HookMessage) {
        match hook_message {
            HookMessage::Tcp(message) => {
                if self.steal {
                    self.tcp_steal_handler
                        .handle_hook_message(message, &self.tx)
                        .await
                        .unwrap();
                } else {
                    self.tcp_mirror_handler
                        .handle_hook_message(message, &self.tx)
                        .await
                        .unwrap();
                }
            }
            HookMessage::File(message) => {
                self.file_handler
                    .handle_hook_message(message, &self.tx)
                    .await
                    .unwrap();
            }
            HookMessage::GetAddrinfo(GetAddrInfo {
                node,
                hook_channel_tx,
            }) => {
                self.getaddrinfo_handler_queue.push_back(hook_channel_tx);
                let request = ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node });

                self.send(request).await.unwrap();
            }
            HookMessage::TcpOutgoing(message) => self
                .tcp_outgoing_handler
                .handle_hook_message(message, &self.tx)
                .await
                .unwrap(),
            HookMessage::UdpOutgoing(message) => self
                .udp_outgoing_handler
                .handle_hook_message(message, &self.tx)
                .await
                .unwrap(),
        }
    }

    /// Passes most of the [`DaemonMessage`]s to their appropriate handlers.
    ///
    /// ## Special case
    ///
    /// ### [`DaemonMessage::Pong`]
    ///
    /// This message has no dedicated handler, and is thus handled directly here, changing the
    /// [`Self::ping`] state.
    ///
    /// ### [`DaemonMessage::GetEnvVarsResponse`]
    ///
    /// Handled during mirrord-layer initialization, this message should never make it this far.
    ///
    /// ### [`DaemonMessage::GetAddrInfoResponse`]
    ///
    /// Also (somewhat) dealt with here, as there is no dedicated handler for it. We just pass the
    /// response along in one of the feature's channels from
    /// `Self::getaddrinfo_handler_queue`.
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
            DaemonMessage::Close(error_message) => Err(LayerError::AgentErrorClosed(error_message)),
            DaemonMessage::LogMessage(_) => todo!(),
        }
    }
}

/// Main loop of mirrord-layer.
///
/// ## Parameters
///
/// - `receiver`: receives the [`HookMessage`]s and pass them along with
///   `Layer::handle_hook_message`;
///
/// - `tx`, `rx`: see `Layer::tx`](link), and [`Layer::rx`;
///
/// - `config`: the mirrord-layer configuration, see [`LayerConfig`].
///
/// ## Details
///
/// We have our big loop here that is initialized in a separate task by [`start_layer_thread`].
///
/// In this loop we:
///
/// - Pass [`HookMessage`]s to be handled by the [`Layer`];
///
/// - Read from the [`Layer`] feature handlers [`Receiver`]s, to turn outgoing messages into
///   [`ClientMessage`]s, sending them with `Layer::send`;
///
/// - Handle the heartbeat mechanism (Ping/Pong feature), sending a [`ClientMessage::Ping`] if all
///   the other channels received nothing for a while (60 seconds);
///
/// - Write log file if `FeatureConfig::capture_error_trace` is set.
async fn thread_loop(
    mut receiver: Receiver<HookMessage>,
    tx: Sender<ClientMessage>,
    rx: Receiver<DaemonMessage>,
    config: LayerConfig,
) {
    let LayerConfig {
        feature:
            FeatureConfig {
                network: NetworkConfig { incoming, .. },
                capture_error_trace,
                ..
            },
        ..
    } = config;
    let mut layer = Layer::new(tx, rx, incoming);
    loop {
        select! {
            hook_message = receiver.recv() => {
                layer.handle_hook_message(hook_message.unwrap()).await;
            }
            Some(tcp_outgoing_message) = layer.tcp_outgoing_handler.recv() => {
                if let Err(fail) =
                    layer.send(ClientMessage::TcpOutgoing(tcp_outgoing_message)).await {
                        error!("Error sending client message: {:#?}", fail);
                        break;
                    }
            }
            Some(udp_outgoing_message) = layer.udp_outgoing_handler.recv() => {
                if let Err(fail) =
                    layer.send(ClientMessage::UdpOutgoing(udp_outgoing_message)).await {
                        error!("Error sending client message: {:#?}", fail);
                        break;
                    }
            }
            daemon_message = layer.rx.recv() => {
                match daemon_message {
                    Some(message) => {
                        match layer.handle_daemon_message(message).await {
                            Err(LayerError::SendErrorConnection(_)) => {
                                info!("Connection closed by agent");
                                continue;
                            }
                            Err(LayerError::AppClosedConnection(connection_unsubscribe)) => {
                                layer.send(connection_unsubscribe).await.unwrap();
                            }
                            Err(err) => {
                                error!("Error handling daemon message: {:?}", err);
                                break;
                            }
                            Ok(()) => {}
                        }
                    },
                    None => {
                        error!("agent connection lost");
                        break;
                    }
                }
            },
            Some(message) = layer.tcp_steal_handler.next() => {
                layer.send(message).await.unwrap();
            }
            Some(response) = layer.http_response_receiver.recv() => {
                layer.send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(response))).await.unwrap();
            }
            _ = sleep(Duration::from_secs(60)) => {
                if !layer.ping {
                    layer.send(ClientMessage::Ping).await.unwrap();
                    trace!("sent ping to daemon");
                    layer.ping = true;
                } else {
                    panic!("Client: unmatched ping");
                }
            }
        }
    }

    if capture_error_trace {
        tracing_util::print_support_message();
    }
    graceful_exit!("mirrord has encountered an error and is now exiting.");
}

/// Initializes mirrord-layer's [`thread_loop`] in a separate [`tokio::task`].
#[tracing::instrument(level = "trace", skip(tx, rx, receiver))]
async fn start_layer_thread(
    tx: Sender<ClientMessage>,
    rx: Receiver<DaemonMessage>,
    receiver: Receiver<HookMessage>,
    config: LayerConfig,
) {
    tokio::spawn(thread_loop(receiver, tx, rx, config));
}

/// Prepares the [`HookManager`] and [`replace!`]s [`libc`] calls with our hooks, according to what
/// the user configured.
///
/// ## Parameters
///
/// - `enabled_file_ops`: replaces [`libc`] file-ish calls with our own from [`file::hooks`], see
///   `FsConfig::is_active`, and [`hooks::enable_file_hooks`](file::hooks::enable_file_hooks);
///
/// - `enabled_remote_dns`: replaces [`libc::getaddrinfo`] and [`libc::freeaddrinfo`] when this is
///   `true`, see [`NetworkConfig`], and
///   [`hooks::enable_socket_hooks`](socket::hooks::enable_socket_hooks).
#[tracing::instrument(level = "trace")]
fn enable_hooks(enabled_file_ops: bool, enabled_remote_dns: bool, patch_binaries: Vec<String>) {
    let mut hook_manager = HookManager::default();

    unsafe {
        replace!(&mut hook_manager, "close", close_detour, FnClose, FN_CLOSE);
        replace!(
            &mut hook_manager,
            "close$NOCANCEL",
            close_nocancel_detour,
            FnClose_nocancel,
            FN_CLOSE_NOCANCEL
        );
        // Solve leak on uvloop which calls the syscall directly.
        #[cfg(target_os = "linux")]
        replace!(
            &mut hook_manager,
            "uv_fs_close",
            uv_fs_close,
            FnUv_fs_close,
            FN_UV_FS_CLOSE
        );
    };

    unsafe { socket::hooks::enable_socket_hooks(&mut hook_manager, enabled_remote_dns) };

    #[cfg(target_os = "macos")]
    unsafe {
        exec::enable_execve_hook(&mut hook_manager, patch_binaries)
    };

    if enabled_file_ops {
        unsafe { file::hooks::enable_file_hooks(&mut hook_manager) };
    }

    #[cfg(target_os = "linux")]
    #[cfg(target_arch = "x86_64")]
    {
        go_hooks::enable_hooks(&mut hook_manager);
    }
}

/// Shared code for closing `fd` in our data structures.
///
/// Callers should call their respective close before calling this.
///
/// ## Details
///
/// Removes the `fd` key from either [`SOCKETS`] or [`OPEN_FILES`].
pub(crate) fn close_layer_fd(fd: c_int) {
    trace!("Closing fd {}", fd);
    let file_mode_active = FILE_MODE
        .get()
        .expect("Should be set during initialization!")
        .is_active();

    // Remove from sockets, also removing the `ConnectionQueue` associated with the socket.
    if let Some(socket) = SOCKETS.lock().unwrap().remove(&fd) {
        CONNECTION_QUEUE.lock().unwrap().remove(socket.id);
    } else if file_mode_active {
        OPEN_FILES.lock().unwrap().remove(&fd);
    }
}

// TODO: When this is annotated with `hook_guard_fn`, then the outgoing sockets never call it (we
// just bypass). Everything works, so, should we intervene?
//
/// Attempts to close on a managed `Socket`, if there is no socket with `fd`, then this means we
/// either let the `fd` bypass and call [`libc::close`] directly, or it might be a managed file
/// `fd`, so it tries to do the same for files.
///
/// ## Hook
///
/// Replaces [`libc::close`].
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    let res = FN_CLOSE(fd);
    close_layer_fd(fd);
    res
}

// TODO(alex) [mid] 2023-01-24: What is this?
///
/// No need to guard because we call another detour which will do the guard for us.
///
/// ## Hook
///
/// Replaces `?`.
#[hook_fn]
pub(crate) unsafe extern "C" fn close_nocancel_detour(fd: c_int) -> c_int {
    close_detour(fd)
}

// TODO(alex) [mid] 2023-01-24: What is this?
///
/// ## Hook
///
/// Replaces `?`.
#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn uv_fs_close(a: usize, b: usize, fd: c_int, c: usize) -> c_int {
    // In this case we call `close_layer_fd` before the original close function, because execution
    // does not return to here after calling `FN_UV_FS_CLOSE`.
    close_layer_fd(fd);
    FN_UV_FS_CLOSE(a, b, fd, c)
}
