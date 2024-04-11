#![feature(c_variadic)]
#![feature(naked_functions)]
#![feature(io_error_uncategorized)]
#![feature(let_chains)]
#![feature(try_trait_v2)]
#![feature(try_trait_v2_residual)]
#![feature(c_size_t)]
#![feature(lazy_cell)]
#![feature(once_cell_try)]
#![allow(rustdoc::private_intra_doc_links)]
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
//! [Configuration](https://mirrord.dev/docs/reference/configuration/) for usage information.

extern crate alloc;
extern crate core;

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    ffi::OsString,
    net::SocketAddr,
    panic,
    sync::OnceLock,
    time::Duration,
};

use ctor::ctor;
use error::{LayerError, Result};
use file::OPEN_FILES;
use hooks::HookManager;
use libc::{c_int, pid_t};
use load::ExecutableName;
#[cfg(target_os = "macos")]
use mirrord_config::feature::fs::FsConfig;
use mirrord_config::{
    feature::{fs::FsModeConfig, network::incoming::IncomingMode},
    LayerConfig,
};
use mirrord_intproxy_protocol::NewSessionRequest;
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::{EnvVars, GetEnvVarsRequest};
use proxy_connection::ProxyConnection;
use setup::LayerSetup;
use socket::SOCKETS;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    common::make_proxy_request_with_response, debugger_ports::DebuggerPorts, detour::DetourGuard,
    load::LoadType,
};

mod common;
mod debugger_ports;
mod detour;
mod error;
#[cfg(target_os = "macos")]
mod exec_utils;
mod file;
mod hooks;
mod load;
mod macros;
mod proxy_connection;
mod setup;
mod socket;

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

// TODO: We don't really need a lock, we just need a type that:
//  1. Can be initialized as static (with a const constructor or whatever)
//  2. Is `Sync` (because shared static vars have to be).
//  3. Can replace the held [`ProxyConnection`] with a different one (because we need to reset it on
//     `fork`).
//  We only ever set it in the ctor or in the `fork` hook (in the child process), and in both cases
//  there are no other threads yet in that process, so we don't need write synchronization.
//  Assuming it's safe to call `send` simultaneously from two threads, on two references to the
//  same `Sender` (is it), we also don't need read synchronization.
/// Global connection to the internal proxy.
/// Should not be used directly. Use [`common::make_proxy_request_with_response`] or
/// [`common::make_proxy_request_no_response`] functions instead.
static mut PROXY_CONNECTION: OnceLock<ProxyConnection> = OnceLock::new();

static SETUP: OnceLock<LayerSetup> = OnceLock::new();

fn setup() -> &'static LayerSetup {
    SETUP.get().expect("layer is not initialized")
}

// The following statics are to avoid using CoreFoundation or high level macOS APIs
// that aren't safe to use after fork.

/// Executable we're loaded to
static EXECUTABLE_NAME: OnceLock<ExecutableName> = OnceLock::new();

/// Executable path we're loaded to
static EXECUTABLE_PATH: OnceLock<String> = OnceLock::new();

/// Program arguments
static EXECUTABLE_ARGS: OnceLock<Vec<OsString>> = OnceLock::new();

/// Proxy Connection timeout
/// Set to 10 seconds as most agent operations timeout after 5 seconds
const PROXY_CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

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
            tracing::error!("Couldn't execute {:?}", err);
            return Err(LayerError::ExecFailed(err));
        }
    }

    match given_process.load_type(&config) {
        LoadType::Full => layer_start(config),
        #[cfg(target_os = "macos")]
        LoadType::SIPOnly => sip_only_layer_start(config, patch_binaries),
        LoadType::Skip => load_only_layer_start(&config),
    }

    Ok(())
}

/// Initialize a new session with the internal proxy and set [`PROXY_CONNECTION`]
/// if not in trace only mode.
fn load_only_layer_start(config: &LayerConfig) {
    // Check if we're in trace only mode (no agent)
    let trace_only = std::env::var(TRACE_ONLY_ENV)
        .unwrap_or_default()
        .parse()
        .unwrap_or(false);
    if trace_only {
        return;
    }

    let address: SocketAddr = config
        .connect_tcp
        .as_ref()
        .expect("missing internal proxy address")
        .parse()
        .expect("failed to parse internal proxy address");

    let new_connection =
        ProxyConnection::new(address, NewSessionRequest::New, PROXY_CONNECTION_TIMEOUT)
            .expect("failed to initialize proxy connection");

    unsafe {
        // SAFETY
        // Called only from library constructor.
        PROXY_CONNECTION
            .set(new_connection)
            .expect("setting PROXY_CONNECTION singleton")
    }
}

/// The one true start of mirrord-layer.
///
/// Calls [`layer_pre_initialization`], which runs mirrord-layer.
#[ctor]
fn mirrord_layer_entry_point() {
    if cfg!(test) {
        return;
    }

    let res = panic::catch_unwind(|| match layer_pre_initialization() {
        Err(LayerError::NoProcessFound) => {}
        Err(e) => {
            eprintln!("mirrord layer setup failed with {e:?}");
            std::process::exit(-1)
        }
        Ok(()) => {}
    });

    if res.is_err() {
        eprintln!("mirrord layer setup panicked");
        std::process::exit(-1);
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

/// Occurs after [`layer_pre_initialization`] has succeeded.
///
/// Initialized the main parts of mirrord-layer.
///
/// ## Details
///
/// Sets up a few things based on the [`LayerConfig`] given by the user:
///
/// 1. [`tracing_subscriber`] or [`mirrord_console`];
///
/// 2. Global [`SETUP`];
///
/// 3. Global [`PROXY_CONNECTION`];
///
/// 4. Replaces the [`libc`] calls with our hooks with [`enable_hooks`];
///
/// 5. Fetches remote environment from the agent (if enabled with
/// [`EnvFileConfig::load_from_process`](mirrord_config::feature::env::EnvFileConfig::load_from_process)).
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

    init_tracing();

    let debugger_ports = DebuggerPorts::from_env();
    let state = LayerSetup::new(config, debugger_ports, trace_only);
    SETUP.set(state).unwrap();

    let state = setup();
    enable_hooks(
        state.fs_config().is_active(),
        state.remote_dns_enabled(),
        state.sip_binaries(),
    );

    let _detour_guard = DetourGuard::new();
    tracing::info!("Initializing mirrord-layer!");
    tracing::trace!(
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
        tracing::debug!("Skipping new intproxy connection (trace only)");
        return;
    }

    unsafe {
        let address = setup().proxy_address();
        let new_connection =
            ProxyConnection::new(address, NewSessionRequest::New, PROXY_CONNECTION_TIMEOUT)
                .expect("failed to initialize proxy connection");
        PROXY_CONNECTION
            .set(new_connection)
            .expect("setting PROXY_CONNECTION singleton")
    }

    let fetch_env = setup().env_config().load_from_process.unwrap_or(false)
        && !std::env::var(REMOTE_ENV_FETCHED)
            .unwrap_or_default()
            .parse::<bool>()
            .unwrap_or(false);
    if fetch_env {
        let env = fetch_env_vars();
        for (key, value) in env {
            std::env::set_var(key, value);
        }

        std::env::set_var(REMOTE_ENV_FETCHED, "true");
    }

    if let Some(unset) = setup().env_config().unset.as_ref() {
        unset.as_slice().iter().for_each(|var| {
            std::env::remove_var(var);
        })
    }
}

/// Name of environment variable used to mark whether remote environment has already been fetched.
const REMOTE_ENV_FETCHED: &str = "MIRRORD_REMOTE_ENV_FETCHED";

/// Fetches remote environment from the agent.
/// Uses [`SETUP`] and [`PROXY_CONNECTION`] globals.
fn fetch_env_vars() -> HashMap<String, String> {
    let (env_vars_exclude, env_vars_include) = match (
        setup()
            .env_config()
            .exclude
            .clone()
            .map(|exclude| exclude.join(";")),
        setup()
            .env_config()
            .include
            .clone()
            .map(|include| include.join(";")),
    ) {
        (Some(..), Some(..)) => {
            panic!("invalid env config");
        }
        (Some(exclude), None) => (HashSet::from(EnvVars(exclude)), HashSet::new()),
        (None, Some(include)) => (HashSet::new(), HashSet::from(EnvVars(include))),
        (None, None) => (HashSet::new(), HashSet::from(EnvVars("*".to_owned()))),
    };

    if !env_vars_exclude.is_empty() || !env_vars_include.is_empty() {
        let mut remote_env = make_proxy_request_with_response(GetEnvVarsRequest {
            env_vars_filter: env_vars_exclude,
            env_vars_select: env_vars_include,
        })
        .expect("failed to make request to proxy")
        .expect("failed to fetch remote env");

        if let Some(overrides) = setup().env_config().r#override.as_ref() {
            remote_env.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        remote_env
    } else {
        Default::default()
    }
}

/// We need to hook execve syscall to allow mirrord-layer to be loaded with sip patch when loading
/// mirrord-layer on a process where specified to skip with MIRRORD_SKIP_PROCESSES
#[cfg(target_os = "macos")]
fn sip_only_layer_start(mut config: LayerConfig, patch_binaries: Vec<String>) {
    load_only_layer_start(&config);

    let mut hook_manager = HookManager::default();

    unsafe { exec_utils::enable_execve_hook(&mut hook_manager, patch_binaries) };

    // we need to hook file access to patch path to our temp bin.
    config.feature.fs = FsConfig {
        mode: FsModeConfig::Local,
        read_write: None,
        read_only: None,
        local: None,
        not_found: None,
    };
    let debugger_ports = DebuggerPorts::from_env();
    let setup = LayerSetup::new(config, debugger_ports, true);

    SETUP.set(setup).expect("SETUP set failed");

    unsafe { file::hooks::enable_file_hooks(&mut hook_manager) };
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
#[mirrord_layer_macro::instrument(level = "trace")]
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
#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn close_layer_fd(fd: c_int) {
    // Remove from sockets.
    if let Some((_, socket)) = SOCKETS.remove(&fd) {
        // Closed file is a socket, so if it's already bound to a port - notify agent to stop
        // mirroring/stealing that port.
        socket.close();
    } else if setup().fs_config().is_active() {
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
    tracing::debug!("Process {} forking!.", std::process::id());

    let res = FN_FORK();

    match res.cmp(&0) {
        Ordering::Equal => {
            tracing::debug!("Child process initializing layer.");
            let parent_connection = match unsafe { PROXY_CONNECTION.take() } {
                Some(conn) => conn,
                None => {
                    tracing::debug!("Skipping new inptroxy connection (trace only)");
                    return res;
                }
            };

            let new_connection = ProxyConnection::new(
                parent_connection.proxy_addr(),
                NewSessionRequest::Forked(parent_connection.layer_id()),
                PROXY_CONNECTION_TIMEOUT,
            )
            .expect("failed to establish proxy connection for child");
            PROXY_CONNECTION
                .set(new_connection)
                .expect("Failed setting PROXY_CONNECTION in child fork");
            // in macOS (and tbh sounds logical) we can't just drop the old connection in the child,
            // as it needs to access a mutex with invalid state, so we need to forget it.
            // better implementation would be to somehow close the underlying connections
            // but side effect should be trivial
            std::mem::forget(parent_connection);
        }
        Ordering::Greater => tracing::debug!("Child process id is {res}."),
        Ordering::Less => tracing::debug!("fork failed"),
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
