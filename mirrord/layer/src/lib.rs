// NOTE(gabriela): prevent compiling lib.rs so layer acts as empty
// library to allow layer-lib as optional dependency for unix.
#![cfg(unix)]
#![feature(c_variadic)]
#![feature(io_error_uncategorized)]
#![feature(try_trait_v2)]
#![feature(try_trait_v2_residual)]
#![feature(c_size_t)]
#![feature(once_cell_try)]
#![feature(vec_into_raw_parts)]
#![allow(rustdoc::private_intra_doc_links)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

//! Loaded dynamically with your local process.
//!
//! Paired with [`mirrord-agent`], it makes your local process behave as if it was running in a
//! remote context.
//!
//! Check out the [Introduction](https://metalbear.com/mirrord/docs/overview/introduction/) guide to learn
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
//!    [`file::ops::open`];
//!
//! 2. [`file::ops::open`] sends an open file message to `mirrord-agent`;
//!
//! 3. `mirrore-agent` tries to open `/tmp/hello.txt` in the remote context it's running, and
//!    returns the result of the operation back to `mirrord-layer`;
//!
//! 4. We handle the mapping of the remote file (the one we have open in `mirrord-agent`), and a
//!    local file (temporarily created);
//!
//! 5. And finally, we return the expected result (type) to your Node.js application, as if it had
//!    just called [`libc::open`].
//!
//! Your application will get an fd that is valid in the context of mirrord, and calls to other file
//! functions (like [`libc::read`]), will work just fine, operating on the remote file.
//!
//! ## Configuration
//!
//! The functions we intercept are controlled via the `mirrord-config` crate, check its
//! documentation for more details, or
//! [Configuration](https://metalbear.com/mirrord/docs/reference/configuration/) for usage information.

extern crate alloc;
extern crate core;

#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
use std::ffi::c_void;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    net::SocketAddr,
    os::unix::process::parent_id,
    panic,
    sync::OnceLock,
    time::Duration,
};

use ctor::ctor;
use error::{LayerError, Result};
use file::OPEN_FILES;
use hooks::HookManager;
#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
use libc::c_char;
use libc::{c_int, pid_t};
use load::ExecuteArgs;
#[cfg(target_os = "macos")]
use mirrord_config::feature::fs::FsConfig;
use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR,
    feature::{env::mapper::EnvVarsRemapper, fs::FsModeConfig, network::incoming::IncomingMode},
};
use mirrord_intproxy_protocol::NewSessionRequest;
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::{EnvVars, GetEnvVarsRequest};
use nix::errno::Errno;
use proxy_connection::ProxyConnection;
use setup::LayerSetup;
use socket::SOCKETS;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    common::make_proxy_request_with_response,
    debugger_ports::DebuggerPorts,
    detour::DetourGuard,
    load::LoadType,
    socket::{hooks::MANAGED_ADDRINFO, ops::REMOTE_DNS_REVERSE_MAPPING},
};

/// Silences `deny(unused_crate_dependencies)`.
///
/// These dependencies are only used in integration tests.
#[cfg(test)]
mod integration_tests_deps {
    use actix_codec as _;
    #[cfg(target_os = "macos")]
    use apple_codesign as _;
    use futures as _;
    use mirrord_intproxy as _;
    use mirrord_tests as _;
    use serde_json as _;
    use tempfile as _;
    use test_cdylib as _;
    use tokio as _;
}

mod common;
mod debugger_ports;
mod detour;
mod error;
mod exec_hooks;
#[cfg(target_os = "macos")]
mod exec_utils;
mod file;
mod hooks;
mod load;
mod macros;
mod proxy_connection;
mod setup;
mod socket;
#[cfg(target_os = "macos")]
mod tls;

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

/// if this env var exists, we exit.
/// This to allow a way to protect from mirrord being used in destructive tests and such.
const FAILSAFE_ENV: &str = "MIRRORD_DONT_LOAD";

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

/// Executable information (name, args)
static EXECUTABLE_ARGS: OnceLock<ExecuteArgs> = OnceLock::new();

/// Executable path we're loaded to
static EXECUTABLE_PATH: OnceLock<String> = OnceLock::new();

/// Read/write timeout for layer<->intproxy TCP sockets.
/// Can be configured in the [`LayerConfig`].
static PROXY_CONNECTION_TIMEOUT: OnceLock<Duration> = OnceLock::new();

/// Loads mirrord configuration and does some patching (SIP, dotnet, etc)
fn layer_pre_initialization() -> Result<(), LayerError> {
    // we don't care about value, just that this env exists
    let dont_start = std::env::var(FAILSAFE_ENV).is_ok();
    if dont_start {
        panic!("{FAILSAFE_ENV} environment variable found, stopping execution.")
    }

    let given_process = EXECUTABLE_ARGS.get_or_try_init(ExecuteArgs::from_env)?;

    EXECUTABLE_PATH.get_or_try_init(|| {
        std::env::current_exe().map(|arg| arg.to_string_lossy().into_owned())
    })?;

    let config = mirrord_config::util::read_resolved_config()?;

    #[cfg(target_os = "macos")]
    let patch_binaries = config
        .sip_binaries
        .clone()
        .map(|x| x.to_vec())
        .unwrap_or_default();

    #[cfg(target_os = "macos")]
    let skip_patch_binaries = config.skip_sip.to_vec();

    // SIP Patch the process' binary then re-execute it. Needed
    // for https://github.com/metalbear-co/mirrord/issues/1529
    #[cfg(target_os = "macos")]
    if given_process.ends_with("dotnet") {
        let path = EXECUTABLE_PATH
            .get()
            .expect("EXECUTABLE_PATH needs to be set!");
        let args = EXECUTABLE_ARGS
            .get()
            .expect("EXECUTABLE_ARGS needs to be set!")
            .args
            .clone();
        let load_type = match &given_process.load_type(&config) {
            LoadType::Full => "full",
            LoadType::SIPOnly => "SIP only",
            LoadType::Skip => "skip",
        };
        let log_info = config
            .experimental
            .sip_log_destination
            .as_ref()
            .map(|log_destination| mirrord_sip::SipLogInfo {
                log_destination,
                args: Some(args.as_slice()),
                load_type: Some(load_type),
            });

        if let Ok(Some(binary)) = mirrord_sip::sip_patch(
            path,
            mirrord_sip::SipPatchOptions {
                patch: &patch_binaries,
                skip: &skip_patch_binaries,
            },
            log_info,
        ) {
            let err = exec::execvp(binary, args);
            tracing::error!("Couldn't execute {:?}", err);
            return Err(LayerError::ExecFailed(err));
        }
    }

    match given_process.load_type(&config) {
        LoadType::Full => layer_start(config),
        #[cfg(target_os = "macos")]
        LoadType::SIPOnly => sip_only_layer_start(config, patch_binaries, skip_patch_binaries),
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

    let address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
        .expect("missing internal proxy address")
        .parse::<SocketAddr>()
        .expect("malformed internal proxy address");

    let new_connection = ProxyConnection::new(
        address,
        NewSessionRequest {
            process_info: EXECUTABLE_ARGS
                .get()
                .expect("EXECUTABLE_ARGS MUST BE SET")
                .to_process_info(config),
            parent_layer: None,
        },
        *PROXY_CONNECTION_TIMEOUT
            .get_or_init(|| Duration::from_secs(config.internal_proxy.socket_timeout)),
    )
    .expect("failed to initialize proxy connection");

    unsafe {
        // SAFETY
        // Called only from library constructor.
        #[allow(static_mut_refs)]
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
///    [`EnvFileConfig::load_from_process`](mirrord_config::feature::env::EnvFileConfig::load_from_process)).
fn layer_start(mut config: LayerConfig) {
    if config.target.path.is_none() && config.feature.fs.mode.ne(&FsModeConfig::Local) {
        // Use localwithoverrides on targetless regardless of user config, unless fs-mode is already
        // set to local.
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
        config.feature.network.dns.enabled = false;
        config.feature.network.incoming.mode = IncomingMode::Off;
        config.feature.network.outgoing.tcp = false;
        config.feature.network.outgoing.udp = false;
    }

    init_tracing();

    let proxy_connection_timeout = *PROXY_CONNECTION_TIMEOUT
        .get_or_init(|| Duration::from_secs(config.internal_proxy.socket_timeout));

    let debugger_ports = DebuggerPorts::from_env();
    let local_hostname = trace_only || !config.feature.hostname;
    let process_info = EXECUTABLE_ARGS
        .get()
        .expect("EXECUTABLE_ARGS MUST BE SET")
        .to_process_info(&config);
    let state = LayerSetup::new(config, debugger_ports, local_hostname);
    SETUP.set(state).unwrap();

    let state = setup();
    enable_hooks(state);

    let _detour_guard = DetourGuard::new();

    // remove resolved encoded config from env vars when logging them
    let env_vars_print_only: Vec<_> = std::env::vars()
        .filter(|(k, _v)| k != LayerConfig::RESOLVED_CONFIG_ENV)
        .collect();
    tracing::info!("Initializing mirrord-layer!");
    tracing::debug!(
        executable = ?EXECUTABLE_PATH.get(),
        args = ?EXECUTABLE_ARGS.get(),
        pid = std::process::id(),
        parent_pid = parent_id(),
        env_vars = ?env_vars_print_only,
        "Loaded into executable (base64 config omitted)",
    );

    if trace_only {
        tracing::debug!("Skipping new intproxy connection (trace only)");
        return;
    }

    #[allow(static_mut_refs)]
    unsafe {
        let address = setup().proxy_address();
        let new_connection = ProxyConnection::new(
            address,
            NewSessionRequest {
                process_info,
                parent_layer: None,
            },
            proxy_connection_timeout,
        )
        .unwrap_or_else(|_| panic!("failed to initialize proxy connection at {address}"));
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
            // TODO: Audit that the environment access only happens in single-threaded code.
            unsafe { std::env::set_var(key, value) };
        }

        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(REMOTE_ENV_FETCHED, "true") };
    }

    if let Some(unset) = setup().env_config().unset.as_ref() {
        let unset = unset.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        std::env::vars().for_each(|(key, _)| {
            if unset.contains(&key.to_lowercase()) {
                // TODO: Audit that the environment access only happens in single-threaded code.
                unsafe { std::env::remove_var(&key) };
            }
        });
    }

    #[cfg(target_os = "macos")]
    if setup().experimental().applev.as_ref().is_some() {
        unsafe {
            let mut applev = exec_utils::extract_applev();
            let mut count: usize = 0;
            while !(*applev).is_null() {
                let c_str = std::ffi::CStr::from_ptr(*applev);
                tracing::info!("applev[{}]: {}", count, c_str.to_string_lossy());
                applev = applev.add(1);
                count = count.saturating_add(1);
            }
            tracing::info!(count, "Finished reading Apple variables");
        }
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

    let mut env_vars = if !env_vars_exclude.is_empty() || !env_vars_include.is_empty() {
        {
            make_proxy_request_with_response(GetEnvVarsRequest {
                env_vars_filter: env_vars_exclude,
                env_vars_select: env_vars_include,
            })
            .expect("failed to make request to proxy")
            .expect("failed to fetch remote env")
        }
    } else {
        Default::default()
    };

    if let Some(file) = &setup().env_config().env_file {
        let envs_from_file = dotenvy::from_path_iter(file)
            .and_then(|iter| iter.collect::<Result<Vec<_>, _>>())
            .expect("failed to access the env file");

        env_vars.extend(envs_from_file);
    }

    if let Some(mapping) = setup().env_config().mapping.clone() {
        env_vars = EnvVarsRemapper::new(mapping, env_vars)
            .expect("Failed creating regex, this should've been caught when verifying config!")
            .remapped();
    }

    if let Some(overrides) = setup().env_config().r#override.as_ref() {
        env_vars.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    env_vars
}

/// We need to hook execve syscall to allow mirrord-layer to be loaded with sip patch when loading
/// mirrord-layer on a process where specified to skip with MIRRORD_SKIP_PROCESSES
#[cfg(target_os = "macos")]
fn sip_only_layer_start(
    mut config: LayerConfig,
    patch_binaries: Vec<String>,
    skip_patch_binaries: Vec<String>,
) {
    use mirrord_config::feature::fs::READONLY_FILE_BUFFER_DEFAULT;

    load_only_layer_start(&config);

    let mut hook_manager = HookManager::default();

    unsafe {
        exec_utils::enable_macos_hooks(&mut hook_manager, patch_binaries, skip_patch_binaries)
    };
    unsafe { exec_hooks::hooks::enable_exec_hooks(&mut hook_manager) };
    unsafe { replace!(&mut hook_manager, "vfork", vfork_detour, FnVfork, FN_VFORK) };

    // we need to hook file access to patch path to our temp bin.
    config.feature.fs = FsConfig {
        mode: FsModeConfig::Local,
        read_write: None,
        read_only: None,
        local: None,
        not_found: None,
        mapping: None,
        readonly_file_buffer: READONLY_FILE_BUFFER_DEFAULT,
    };
    let debugger_ports = DebuggerPorts::from_env();
    let layer_setup = LayerSetup::new(config, debugger_ports, true);

    SETUP.set(layer_setup).expect("SETUP set failed");

    unsafe { file::hooks::enable_file_hooks(&mut hook_manager, setup()) };

    if let Some(unset) = setup().env_config().unset.as_ref() {
        let unset = unset.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        std::env::vars().for_each(|(key, _)| {
            if unset.contains(&key.to_lowercase()) {
                unsafe { std::env::remove_var(&key) };
            }
        });
    }
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
///   `true`, see [`NetworkConfig`](mirrord_config::feature::network::NetworkConfig), and
///   [`hooks::enable_socket_hooks`](socket::hooks::enable_socket_hooks).
#[mirrord_layer_macro::instrument(level = tracing::Level::TRACE)]
fn enable_hooks(state: &LayerSetup) {
    let enabled_file_ops = state.fs_config().is_active();
    let enabled_remote_dns = state.remote_dns_enabled();

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
        replace!(&mut hook_manager, "vfork", vfork_detour, FnVfork, FN_VFORK);
    };

    unsafe {
        socket::hooks::enable_socket_hooks(
            &mut hook_manager,
            enabled_remote_dns,
            state.experimental(),
        )
    };

    if cfg!(target_os = "macos") || state.experimental().enable_exec_hooks_linux {
        unsafe { exec_hooks::hooks::enable_exec_hooks(&mut hook_manager) };
    }

    #[cfg(target_os = "macos")]
    {
        use crate::exec_utils::enable_macos_hooks;

        let patch_binaries = state.sip_binaries();
        let skip_patch_binaries = state.skip_patch_binaries();
        unsafe { enable_macos_hooks(&mut hook_manager, patch_binaries, skip_patch_binaries) };

        if state.experimental().trust_any_certificate {
            unsafe { tls::enable_tls_hooks(&mut hook_manager) };
        }
    }

    if enabled_file_ops {
        unsafe { file::hooks::enable_file_hooks(&mut hook_manager, state) };
    }

    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))]
    {
        go_hooks::enable_hooks(&mut hook_manager);
    }

    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))]
    {
        if state.experimental().dlopen_cgo {
            unsafe {
                replace!(
                    &mut hook_manager,
                    "dlopen",
                    dlopen_detour,
                    FnDlopen,
                    FN_DLOPEN
                );
            }
        }
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
#[mirrord_layer_macro::instrument(level = "trace", fields(pid = std::process::id()))]
pub(crate) fn close_layer_fd(fd: c_int) {
    // Remove from sockets.
    match SOCKETS.lock().expect("SOCKETS lock failed").remove(&fd) {
        Some(socket) => {
            // Closed file is a socket, so if it's already bound to a port - notify agent to stop
            // mirroring/stealing that port.
            socket.close();
        }
        _ => {
            if setup().fs_config().is_active() {
                OPEN_FILES
                    .lock()
                    .expect("OPEN_FILES lock failed")
                    .remove(&fd);
            }
        }
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
    unsafe {
        let res = FN_CLOSE(fd);
        close_layer_fd(fd);
        res
    }
}

/// Hook for `libc::fork`.
///
/// on macOS, be wary what we do in this path as we might trigger <https://github.com/metalbear-co/mirrord/issues/1745>
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fork_detour() -> pid_t {
    // when running in multi-threaded app, we can have a scenario where another thread holds a mutex
    // while the fork executes this leaves the mutex locked forever in the child process since
    // there's no thread to unlock it so we need to grab all the mutexes we can here, and drop
    // after the fork see https://github.com/metalbear-co/mirrord/issues/3659#issuecomment-3433990010
    let sockets = SOCKETS.lock();
    let open_files = OPEN_FILES.lock();
    let addr_info = MANAGED_ADDRINFO.lock();
    let dns_mapping = REMOTE_DNS_REVERSE_MAPPING.lock();

    unsafe {
        tracing::debug!("Process {} forking!.", std::process::id());

        let res = FN_FORK();

        match res.cmp(&0) {
            Ordering::Equal => {
                tracing::debug!("Child process initializing layer.");
                #[allow(static_mut_refs)]
                let parent_connection = match PROXY_CONNECTION.take() {
                    Some(conn) => conn,
                    None => {
                        tracing::debug!("Skipping new inptroxy connection (trace only)");
                        return res;
                    }
                };

                let new_connection = ProxyConnection::new(
                    parent_connection.proxy_addr(),
                    NewSessionRequest {
                        parent_layer: Some(parent_connection.layer_id()),
                        process_info: EXECUTABLE_ARGS
                            .get()
                            .expect("should always be set in layer constructor")
                            .to_process_info(setup().layer_config()),
                    },
                    PROXY_CONNECTION_TIMEOUT
                        .get()
                        .copied()
                        .expect("PROXY_CONNECTION_TIMEOUT should be set by now!"),
                )
                .expect("failed to establish proxy connection for child");
                #[allow(static_mut_refs)]
                PROXY_CONNECTION
                    .set(new_connection)
                    .expect("Failed setting PROXY_CONNECTION in child fork");
                // in macOS (and tbh sounds logical) we can't just drop the old connection in the
                // child, as it needs to access a mutex with invalid state, so we
                // need to forget it. better implementation would be to somehow
                // close the underlying connections but side effect should be
                // trivial
                std::mem::forget(parent_connection);
            }
            Ordering::Greater => tracing::debug!("Child process id is {res}."),
            Ordering::Less => tracing::debug!("fork failed"),
        }

        drop(sockets);
        drop(open_files);
        drop(addr_info);
        drop(dns_mapping);
        res
    }
}

/// Transparently replaces `vfork` call with a safer `fork`.
///
/// `vfork` and `fork` calls are very similar (except performance),
/// except that with `vfork` the parent process is blocked until the child exits or execs.
/// This hook uses a dummy pipe to handle this difference:
/// 1. Parent process creates a pipe with O_CLOEXEC flag.
/// 2. Parent process forks.
/// 3. Parent process closes the writing end and blocks on reading from the reading end.
/// 4. The read call returns with EOF when the child process exits or execs.
///
/// # Rationale
///
/// To properly handle child processes, we need to hook functions from the `exec` family.
/// These are often used in tandem with `vfork`. However, `vfork` requires the child process
/// not to write to memory (it's shared with the parent). Doing it in the child process results with
/// an undefined behavior. Since our hooks always write to memory, `vfork` is not our ally.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn vfork_detour() -> pid_t {
    #[cfg(target_os = "macos")]
    let pipe_result = {
        use std::os::fd::AsRawFd;

        use nix::fcntl::{FcntlArg, FdFlag};

        nix::unistd::pipe().and_then(|pipe| {
            nix::fcntl::fcntl(pipe.1.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
            Ok(pipe)
        })
    };
    #[cfg(target_os = "linux")]
    let pipe_result = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC);

    let (pipe_read, pipe_write) = match pipe_result {
        Ok(pipe) => pipe,
        Err(error) => {
            // If we cannot create the pipe, fallback to vfork.
            tracing::error!(
                %error,
                "Failed to create a dummy pipe for vfork emulation. \
                Letting the vfork happen. This might cause unforeseen issues. \
                {}",
                if error == Errno::EMFILE {
                    "Try increasing the process open files limit using `ulimit`."
                } else {
                    "This is most probably a bug, please report it."
                }
            );
            return unsafe { FN_VFORK() };
        }
    };

    let fork_result = unsafe { fork_detour() };
    match fork_result.cmp(&0) {
        // We're the child.
        // Write end of the pipe will be dropped when we exit or exec,
        // notifying the parent.
        Ordering::Equal => {
            std::mem::drop(pipe_read);
        }
        // We're the parent.
        // To emulate vfork properly, we need to wait until the child
        // drops the write end of the pipe.
        // Regardless of what happens now, we return with success.
        // Worst case scenario, the parent will resume without waiting for the child to
        // exit/exec.
        Ordering::Greater => {
            std::mem::drop(pipe_write);
            let mut file = File::from(pipe_read);
            let mut buf = Vec::new();
            match file.read_to_end(&mut buf) {
                Ok(0) => {
                    // Write end dropped, success.
                }
                Ok(..) => {
                    tracing::error!(
                        read_from_dummy_pipe = %String::from_utf8_lossy(&buf),
                        "Unexpected error in vfork emulation. \
                        This is a bug, please report it."
                    )
                }
                Err(error) => {
                    tracing::error!(
                        read_from_dummy_pipe_error = %error,
                        "Unexpected error in vfork emulation. \
                        This is a bug, please report it",
                    );
                }
            }
        }
        // fork failed, fallback to vfork.
        Ordering::Less => {
            std::mem::drop(pipe_read);
            std::mem::drop(pipe_write);
            tracing::error!(
                error = %Errno::from_raw(fork_result),
                "Failed to fork the current process for vfork emulation. \
                Letting the vfork happen. This might cause unforeseen issues.",
            );
            return unsafe { FN_VFORK() };
        }
    }

    fork_result
}

/// No need to guard because we call another detour which will do the guard for us.
///
/// ## Hook
///
/// One of the many [`libc::close`]-ish functions.
#[hook_fn]
pub(crate) unsafe extern "C" fn close_nocancel_detour(fd: c_int) -> c_int {
    unsafe { close_detour(fd) }
}

#[hook_fn]
pub(crate) unsafe extern "C" fn __close_nocancel_detour(fd: c_int) -> c_int {
    unsafe { close_detour(fd) }
}

#[hook_fn]
pub(crate) unsafe extern "C" fn __close_detour(fd: c_int) -> c_int {
    unsafe { close_detour(fd) }
}

/// ## Hook
///
/// Needed for libuv that calls the syscall directly.
/// <https://github.dev/libuv/libuv/blob/7b84d5b0ecb737b4cc30ce63eade690d994e00a6/src/unix/core.c#L557-L558>
#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn uv_fs_close_detour(
    a: usize,
    b: usize,
    fd: c_int,
    c: usize,
) -> c_int {
    unsafe {
        // In this case we call `close_layer_fd` before the original close function, because
        // execution does not return to here after calling `FN_UV_FS_CLOSE`.
        close_layer_fd(fd);
        FN_UV_FS_CLOSE(a, b, fd, c)
    }
}

/// ## Hook
///
/// Detect if `dlopen()` loaded go dynamic library. If so, enable go specific hooks.
#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
#[hook_fn]
pub(crate) unsafe extern "C" fn dlopen_detour(
    raw_path: *const c_char,
    mode: c_int,
) -> *const c_void {
    let handle = unsafe { FN_DLOPEN(raw_path, mode) };
    let _guard = DetourGuard::new();

    let mut hook_manager = HookManager::default();
    let path_str = unsafe {
        std::ffi::CStr::from_ptr(raw_path)
            .to_string_lossy()
            .into_owned()
    };
    let filename = std::path::Path::new(&path_str)
        .file_name()
        .expect("cannot get the filename of the dynamic library")
        .to_string_lossy()
        .into_owned();
    go_hooks::enable_hooks_in_loaded_module(&mut hook_manager, filename);

    handle
}
