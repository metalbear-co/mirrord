#![feature(c_variadic)]
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
#![feature(lazy_cell)]
#![feature(async_fn_in_trait)]
#![feature(once_cell_try)]
#![allow(rustdoc::private_intra_doc_links)]
#![allow(incomplete_features)]
#![warn(clippy::indexing_slicing)]

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

use std::{collections::VecDeque, ffi::OsString, net::SocketAddr, panic, sync::OnceLock};

use bimap::BiMap;
use common::ResponseChannel;
use ctor::ctor;
use dns::GetAddrInfo;
use error::{LayerError, Result};
use file::{filter::FileFilter, OPEN_FILES};
use hooks::HookManager;
use libc::{c_int, pid_t};
use load::ExecutableName;
use mirrord_config::{
    feature::{
        fs::{FsConfig, FsModeConfig},
        network::{
            incoming::{IncomingConfig, IncomingMode},
            NetworkConfig,
        },
        FeatureConfig,
    },
    LayerConfig,
};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest},
    tcp::{HttpResponseFallback, LayerTcpSteal},
    ClientMessage, DaemonMessage, CLIENT_READY_FOR_LOGS,
};
use outgoing::{tcp::TcpOutgoingHandler, udp::UdpOutgoingHandler};
use regex::RegexSet;
use socket::{OutgoingSelector, SOCKETS};
use tcp::TcpHandler;
use tcp_mirror::TcpMirrorHandler;
use tcp_steal::TcpStealHandler;
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    time::{Duration, Interval},
};
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    common::HookMessage,
    debugger_ports::DebuggerPorts,
    detour::DetourGuard,
    file::{filter::FILE_FILTER, FileHandler},
    load::LoadType,
    socket::CONNECTION_QUEUE,
};

mod common;
mod connection;
mod debugger_ports;
mod detour;
mod dns;
mod error;
#[cfg(target_os = "macos")]
mod exec_utils;
mod file;
mod hooks;
mod load;
mod macros;
mod outgoing;
mod socket;
mod tcp;
mod tcp_mirror;
mod tcp_steal;

#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
mod go;

#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
use crate::go::go_hooks;

const TRACE_ONLY_ENV: &str = "MIRRORD_LAYER_TRACE_ONLY";

fn build_runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// TODO: We don't really need a lock, we just need a type that:
//  1. Can be initialized as static (with a const constructor or whatever)
//  2. Is `Sync` (because shared static vars have to be).
//  3. Can replace the held `Sender` with a different one (because we need to reset it on `fork`).
//  We only ever set it in the ctor or in the `fork` hook (in the child process), and in both cases
//  there are no other threads yet in that process, so we don't need write synchronization.
//  Assuming it's safe to call `send` simultaneously from two threads, on two references to the
//  same `Sender` (is it), we also don't need read synchronization.
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
pub(crate) static mut HOOK_SENDER: OnceLock<Sender<HookMessage>> = OnceLock::new();

pub(crate) static LAYER_INITIALIZED: OnceLock<()> = OnceLock::new();

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

/// Unix streams to connect to remotely.
///
/// ## Usage
///
/// Used to change the behavior of the `socket::ops::connect` hook operation.
pub(crate) static REMOTE_UNIX_STREAMS: OnceLock<Option<RegexSet>> = OnceLock::new();

/// Tells us if the user enabled wants to ignore localhots connections in [`OutgoingConfig`].
///
/// ## Usage
///
/// When true, localhost connections will stay local (won't go to the remote pod localhost)
pub(crate) static OUTGOING_IGNORE_LOCALHOST: OnceLock<bool> = OnceLock::new();

/// Incoming config [`IncomingConfig`].
pub(crate) static INCOMING_CONFIG: OnceLock<IncomingConfig> = OnceLock::new();

/// Indicates this is a targetless run, so that users can be warned if their application is
/// mirroring/stealing from a targetless agent.
pub(crate) static TARGETLESS: OnceLock<bool> = OnceLock::new();

/// Whether to resolve DNS locally or through the remote pod.
pub(crate) static REMOTE_DNS: OnceLock<bool> = OnceLock::new();

/// Selector for how outgoing connection will behave, either sending traffic via the remote or from
/// local app, according to how the user set up the `remote`, and `local` filter, in
/// `feature.network.outgoing`.
pub(crate) static OUTGOING_SELECTOR: OnceLock<OutgoingSelector> = OnceLock::new();

/// Ports to ignore because they are used by the IDE debugger
pub(crate) static DEBUGGER_IGNORED_PORTS: OnceLock<DebuggerPorts> = OnceLock::new();

/// Mapping of ports to use for binding local sockets created by us for intercepting.
pub(crate) static LISTEN_PORTS: OnceLock<BiMap<u16, u16>> = OnceLock::new();

// The following statics are to avoid using CoreFoundation or high level macOS APIs
// that aren't safe to use after fork.

/// Executable we're loaded to
pub(crate) static EXECUTABLE_NAME: OnceLock<ExecutableName> = OnceLock::new();

/// Executable path we're loaded to
pub(crate) static EXECUTABLE_PATH: OnceLock<String> = OnceLock::new();

/// Program arguments
pub(crate) static EXECUTABLE_ARGS: OnceLock<Vec<OsString>> = OnceLock::new();

/// Prevent mirrord from connecting to ports used by the IDE debugger
pub(crate) fn is_debugger_port(addr: &SocketAddr) -> bool {
    DEBUGGER_IGNORED_PORTS
        .get()
        .expect("DEBUGGER_IGNORED_PORTS not initialized")
        .contains(addr)
}

/// Loads mirrord configuration and does some patching (SIP, dotnet, etc)
fn layer_pre_initialization() -> Result<(), LayerError> {
    let given_process = EXECUTABLE_NAME.get_or_try_init(ExecutableName::from_env)?;

    EXECUTABLE_PATH.get_or_try_init(|| {
        std::env::current_exe().map(|arg| arg.to_string_lossy().into_owned())
    })?;
    EXECUTABLE_ARGS.get_or_init(|| std::env::args_os().collect());
    let config = LayerConfig::from_env()?;

    #[cfg(target_os = "macos")]
    let patch_binaries = config
        .sip_binaries
        .clone()
        .map(|x| x.to_vec())
        .unwrap_or_default();

    // SIP Patch the process' binary then re-execute it. Needed
    // for https://github.com/metalbear-co/mirrord/issues/1529
    #[cfg(target_os = "macos")]
    if given_process.ends_with("dotnet") {
        let path = EXECUTABLE_PATH
            .get()
            .expect("EXECUTABLE_PATH needs to be set!");
        if let Ok(Some(binary)) = mirrord_sip::sip_patch(path, &patch_binaries) {
            let err = exec::execvp(
                binary,
                EXECUTABLE_ARGS
                    .get()
                    .expect("EXECUTABLE_ARGS needs to be set!"),
            );
            error!("Couldn't execute {:?}", err);
            return Err(LayerError::ExecFailed(err));
        }
    }

    match given_process.load_type(config) {
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

/// Initialize logger. Set the logs to go according to the layer's config either to a trace file, to
/// mirrord-console or to stderr.
fn init_tracing() {
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_logger(&console_addr).expect("logger initialization failed");
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
}

/// Set the shared static variables according to the layer's configuration.
/// These have to be global because they have to be accessed from hooks (which are called by user
/// code in user threads).
///
/// Would panic if any of the variables is already set. This should never be the case.
fn set_globals(config: &LayerConfig) {
    FILE_MODE
        .set(config.feature.fs.clone())
        .expect("Setting FILE_MODE failed.");
    INCOMING_CONFIG
        .set(config.feature.network.incoming.clone())
        .expect("SETTING INCOMING_CONFIG singleton");

    // These must come before `OutgoingSelector::new`.
    ENABLED_TCP_OUTGOING
        .set(config.feature.network.outgoing.tcp)
        .expect("Setting ENABLED_TCP_OUTGOING singleton");
    ENABLED_UDP_OUTGOING
        .set(config.feature.network.outgoing.udp)
        .expect("Setting ENABLED_UDP_OUTGOING singleton");

    REMOTE_DNS
        .set(config.feature.network.dns)
        .expect("Setting REMOTE_DNS singleton");

    {
        let outgoing_selector: OutgoingSelector = config
            .feature
            .network
            .outgoing
            .filter
            .clone()
            .try_into()
            .expect("Failed setting up outgoing traffic filter!");

        // This will crash the app if it comes before `ENABLED_(TCP|UDP)_OUTGOING`!
        OUTGOING_SELECTOR
            .set(outgoing_selector)
            .expect("Setting OUTGOING_SELECTOR singleton");
    }

    REMOTE_UNIX_STREAMS
        .set(
            config
                .feature
                .network
                .outgoing
                .unix_streams
                .clone()
                .map(|vec_or_single| vec_or_single.to_vec())
                .map(RegexSet::new)
                .transpose()
                .expect("Invalid unix stream regex set."),
        )
        .expect("Setting REMOTE_UNIX_STREAMS failed.");

    OUTGOING_IGNORE_LOCALHOST
        .set(config.feature.network.outgoing.ignore_localhost)
        .expect("Setting OUTGOING_IGNORE_LOCALHOST singleton");

    TARGETLESS
        .set(config.target.path.is_none())
        .expect("Setting TARGETLESS singleton");

    FILE_FILTER.get_or_init(|| FileFilter::new(config.feature.fs.clone()));

    DEBUGGER_IGNORED_PORTS
        .set(DebuggerPorts::from_env())
        .expect("Setting DEBUGGER_IGNORED_PORTS failed");

    LISTEN_PORTS
        .set(config.feature.network.incoming.listen_ports.clone())
        .expect("Setting LISTEN_PORTS failed");
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
fn layer_start(mut config: LayerConfig) {
    if config.target.path.is_none() {
        // Use localwithoverrides on targetless regardless of user config.
        config.feature.fs.mode = FsModeConfig::LocalWithOverrides;
    }

    // Check if we're in trace only mode (no agent)
    let trace_only = std::env::var(TRACE_ONLY_ENV)
        .unwrap_or_default()
        .parse()
        .unwrap_or(false);

    // Disable all features that require the agent
    if trace_only {
        config.feature.fs.mode = FsModeConfig::Local;
        config.feature.network.dns = false;
        config.feature.network.incoming.mode = IncomingMode::Off;
        config.feature.network.outgoing.tcp = false;
        config.feature.network.outgoing.udp = false;
    }

    // does not need to be atomic because on the first call there are never other threads.
    // Will be false when manually called from fork hook.
    if LAYER_INITIALIZED.get().is_none() {
        // If we're here it's not a fork, we're in the ctor.
        let _ = LAYER_INITIALIZED.set(());
        init_tracing();
        set_globals(&config);
        enable_hooks(
            config.feature.fs.is_active(),
            config.feature.network.dns,
            config
                .sip_binaries
                .clone()
                .map(|x| x.to_vec())
                .unwrap_or_default(),
        );
    }

    let _detour_guard = DetourGuard::new();
    info!("Initializing mirrord-layer!");
    trace!(
        "Loaded into executable: {}, on pid {}, with args: {:?}",
        EXECUTABLE_PATH
            .get()
            .expect("EXECUTABLE_PATH should be set!"),
        std::process::id(),
        EXECUTABLE_ARGS
            .get()
            .expect("EXECUTABLE_ARGS needs to be set!")
    );

    if trace_only {
        return;
    }

    let address = config
        .connect_tcp
        .as_ref()
        .expect("layer loaded without proxy address to connect to")
        .parse()
        .expect("couldn't parse proxy address");

    let runtime = build_runtime();
    let (tx, rx) = runtime.block_on(connection::connect_to_proxy(address));
    let (sender, receiver) = channel::<HookMessage>(1000);

    // SAFETY: mutation of `HOOK_SENDER` happens only in the ctor, or in the fork hook, and in all
    // of those cases there's only one thread so we can do this safely.
    unsafe {
        if let Some(old_sender) = HOOK_SENDER.take() {
            // HOOK_SENDER is already set, we're currently on a fork detour.

            // we can't call drop since it might trigger the traceback that can be seen here
            // https://github.com/metalbear-co/mirrord/issues/1792
            // so we leak it.
            std::mem::forget(old_sender);
        }
        HOOK_SENDER
            .set(sender)
            .expect("Setting HOOK_SENDER singleton");
    };

    start_layer_thread(tx, rx, receiver, config, runtime);
}

/// We need to hook execve syscall to allow mirrord-layer to be loaded with sip patch when loading
/// mirrord-layer on a process where specified to skip with MIRRORD_SKIP_PROCESSES
#[cfg(target_os = "macos")]
fn sip_only_layer_start(patch_binaries: Vec<String>) {
    let mut hook_manager = HookManager::default();

    unsafe { exec_utils::enable_execve_hook(&mut hook_manager, patch_binaries) };
    // we need to hook file access to patch path to our temp bin.
    FILE_FILTER
        .set(FileFilter::new(FsConfig {
            mode: FsModeConfig::Local,
            read_write: None,
            read_only: None,
            local: None,
        }))
        .expect("FILE_FILTER set failed");
    unsafe { file::hooks::enable_file_hooks(&mut hook_manager) };
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

    ping_interval: Interval,

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
    pub http_response_receiver: Receiver<HttpResponseFallback>,

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
            http_header_filter,
            http_filter,
            port_mapping,
            ..
        } = incoming;

        Self {
            tx,
            rx,
            ping: false,
            ping_interval: tokio::time::interval(Duration::from_secs(30)),
            tcp_mirror_handler: TcpMirrorHandler::new(port_mapping.clone()),
            tcp_outgoing_handler: TcpOutgoingHandler::default(),
            udp_outgoing_handler: Default::default(),
            file_handler: FileHandler::default(),
            getaddrinfo_handler_queue: VecDeque::new(),
            tcp_steal_handler: TcpStealHandler::new(
                http_response_sender,
                port_mapping,
                (http_filter, http_header_filter).into(),
            ),
            http_response_receiver,
            steal,
        }
    }

    /// Sends a [`ClientMessage`] through `Layer::tx` to the [`Receiver`] in
    /// [`wrap_raw_connection`](mirrord_kube::api::wrap_raw_connection).
    async fn send(&mut self, msg: ClientMessage) -> Result<(), ClientMessage> {
        self.ping_interval.reset();
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
    /// ### [`DaemonMessage::GetEnvVarsResponse`] and [`DaemonMessage::PauseTarget`]
    ///
    /// Handled during mirrord-layer initialization, this message should never make it this far.
    ///
    /// ### [`DaemonMessage::GetAddrInfoResponse`]
    ///
    /// Also (somewhat) dealt with here, as there is no dedicated handler for it. We just pass the
    /// response along in one of the feature's channels from
    /// `Self::getaddrinfo_handler_queue`.
    ///
    /// ### [`DaemonMessage::LogMessage`]
    ///
    /// This message is handled in protocol level `wrap_raw_connection`.
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
            DaemonMessage::LogMessage(_) => Ok(()),
            DaemonMessage::PauseTarget(_) => {
                unreachable!("We set pausing target only on initialization, shouldn't happen")
            }
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    if let Err(err) = self.tx.send(ClientMessage::ReadyForLogs).await {
                        warn!("Unable to ready-up for logs: {err}");
                    }
                }

                Ok(())
            }
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
///   the other channels received nothing for a while (30 seconds);
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
                ..
            },
        ..
    } = config;
    let mut layer = Layer::new(tx, rx, incoming);
    layer.ping_interval.tick().await;

    if let Err(err) = layer
        .tx
        .send(ClientMessage::SwitchProtocolVersion(
            mirrord_protocol::VERSION.clone(),
        ))
        .await
    {
        warn!("Unable to switch protocol: {err}");
    }

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
                let message = match response {
                    HttpResponseFallback::Framed(res) => ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(res)),
                    HttpResponseFallback::Fallback(res) => ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(res))
                };

                layer.send(message).await.unwrap();
            }
            _ = layer.ping_interval.tick() => {
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
    graceful_exit!("mirrord has encountered an error and is now exiting.");
}

/// Initializes mirrord-layer's [`thread_loop`] in a separate [`tokio::task`].
#[tracing::instrument(level = "trace", skip(tx, rx, receiver))]
fn start_layer_thread(
    tx: Sender<ClientMessage>,
    rx: Receiver<DaemonMessage>,
    receiver: Receiver<HookMessage>,
    config: LayerConfig,
    runtime: Runtime,
) {
    std::thread::spawn(move || {
        let _guard = DetourGuard::new();
        runtime.block_on(thread_loop(receiver, tx, rx, config))
    });
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

        replace!(
            &mut hook_manager,
            "__close_nocancel",
            __close_nocancel_detour,
            Fn__close_nocancel,
            FN___CLOSE_NOCANCEL
        );

        replace!(
            &mut hook_manager,
            "__close",
            __close_detour,
            Fn__close,
            FN___CLOSE
        );

        // Solve leak on uvloop which calls the syscall directly.
        #[cfg(target_os = "linux")]
        {
            replace!(
                &mut hook_manager,
                "uv_fs_close",
                uv_fs_close_detour,
                FnUv_fs_close,
                FN_UV_FS_CLOSE
            );
        };

        replace!(&mut hook_manager, "fork", fork_detour, FnFork, FN_FORK);
    };

    unsafe { socket::hooks::enable_socket_hooks(&mut hook_manager, enabled_remote_dns) };

    #[cfg(target_os = "macos")]
    unsafe {
        exec_utils::enable_execve_hook(&mut hook_manager, patch_binaries)
    };

    if enabled_file_ops {
        unsafe { file::hooks::enable_file_hooks(&mut hook_manager) };
    }

    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))]
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
/// **DON'T ADD LOGS HERE SINCE CALLER MIGHT CLOSE STDOUT/STDERR CAUSING THIS TO CRASH**
pub(crate) fn close_layer_fd(fd: c_int) {
    let file_mode_active = FILE_MODE
        .get()
        .expect("Should be set during initialization!")
        .is_active();

    // Remove from sockets, also removing the `ConnectionQueue` associated with the socket.
    if let Some((_, socket)) = SOCKETS.remove(&fd) {
        CONNECTION_QUEUE.remove(socket.id);

        // Closed file is a socket, so if it's already bound to a port - notify agent to stop
        // mirroring/stealing that port.
        socket.close();
    } else if file_mode_active {
        OPEN_FILES.remove(&fd);
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

/// Hook for `libc::fork`.
///
/// on macOS, be wary what we do in this path as we might trigger https://github.com/metalbear-co/mirrord/issues/1745
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fork_detour() -> pid_t {
    debug!("Process {} forking!.", std::process::id());
    let res = FN_FORK();
    if res == 0 {
        debug!("Child process initializing layer.");
        mirrord_layer_entry_point()
    } else {
        debug!("Child process id is {res}.");
    }
    res
}

/// No need to guard because we call another detour which will do the guard for us.
///
/// ## Hook
///
/// One of the many [`libc::close`]-ish functions.
#[hook_fn]
pub(crate) unsafe extern "C" fn close_nocancel_detour(fd: c_int) -> c_int {
    close_detour(fd)
}

#[hook_fn]
pub(crate) unsafe extern "C" fn __close_nocancel_detour(fd: c_int) -> c_int {
    close_detour(fd)
}

#[hook_fn]
pub(crate) unsafe extern "C" fn __close_detour(fd: c_int) -> c_int {
    close_detour(fd)
}

/// ## Hook
///
/// Needed for libuv that calls the syscall directly.
/// https://github.dev/libuv/libuv/blob/7b84d5b0ecb737b4cc30ce63eade690d994e00a6/src/unix/core.c#L557-L558
#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn uv_fs_close_detour(
    a: usize,
    b: usize,
    fd: c_int,
    c: usize,
) -> c_int {
    // In this case we call `close_layer_fd` before the original close function, because execution
    // does not return to here after calling `FN_UV_FS_CLOSE`.
    close_layer_fd(fd);
    FN_UV_FS_CLOSE(a, b, fd, c)
}
