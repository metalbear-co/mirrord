//! Unix layer core shared by the full layer and the minimal remote layer.

#![cfg(unix)]
#![feature(once_cell_try)]
#![allow(rustdoc::private_intra_doc_links)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    os::unix::process::parent_id,
    sync::OnceLock,
    time::Duration,
};

pub use hooks::HookManager;
use libc::c_char;
pub use load::{ExecuteArgs, LoadType};
use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, feature::env::mapper::EnvVarsRemapper,
};
use mirrord_intproxy_protocol::NewSessionRequest;
use mirrord_layer_lib::error::{LayerError, Result};
pub use mirrord_layer_lib::{
    detour::DetourGuard,
    logging::init_tracing,
    proxy_connection::{PROXY_CONNECTION, ProxyConnection, make_proxy_request_with_response},
    setup::{LayerSetup, init_layer_setup, setup},
    trace_only::is_trace_only_mode,
};
use mirrord_protocol::{EnvVars, GetEnvVarsRequest};

pub mod hooks;
pub mod load;
pub mod macros;

/// if this env var exists, we exit.
/// This to allow a way to protect from mirrord being used in destructive tests and such.
pub const FAILSAFE_ENV: &str = "MIRRORD_DONT_LOAD";

// The following statics are to avoid using CoreFoundation or high level macOS APIs
// that aren't safe to use after fork.

/// Executable information (name, args)
pub static EXECUTABLE_ARGS: OnceLock<ExecuteArgs> = OnceLock::new();

/// Executable path we're loaded to
pub static EXECUTABLE_PATH: OnceLock<String> = OnceLock::new();

/// Read/write timeout for layer<->intproxy TCP sockets.
/// Can be configured in the [`LayerConfig`].
pub static PROXY_CONNECTION_TIMEOUT: OnceLock<Duration> = OnceLock::new();

/// Result of the generic pre-initialization step.
#[derive(Debug)]
pub struct LayerInitialization {
    /// Resolved layer config.
    pub config: LayerConfig,
    /// Load mode derived from the current process and config.
    pub load_type: LoadType,
}

/// Loads mirrord configuration and records the current process information.
pub fn layer_pre_initialization() -> Result<LayerInitialization, LayerError> {
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
    let load_type = given_process.load_type(&config);

    Ok(LayerInitialization { config, load_type })
}

/// Initialize a new session with the internal proxy and set [`PROXY_CONNECTION`]
/// if not in trace only mode.
pub fn load_only_layer_start(config: &LayerConfig) {
    // Check if we're in trace only mode (no agent)
    if is_trace_only_mode() {
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

/// Shared Unix layer startup used by the full layer.
///
/// This owns the common startup sequencing: tracing, setup, proxy connection, remote-env loading,
/// and the post-start logging that depends on the resolved configuration.
pub fn layer_start(config: LayerConfig, enable_hooks: impl FnOnce(&LayerSetup)) {
    init_tracing();

    let proxy_connection_timeout = *PROXY_CONNECTION_TIMEOUT
        .get_or_init(|| Duration::from_secs(config.internal_proxy.socket_timeout));

    let process_info = EXECUTABLE_ARGS
        .get()
        .expect("EXECUTABLE_ARGS MUST BE SET")
        .to_process_info(&config);

    // initialize LayerSetup from config
    init_layer_setup(config, false);

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

    if is_trace_only_mode() {
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
            let mut applev = extract_applev();
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

#[cfg(target_os = "macos")]
/// Skip all entries in `envp`, extract and return all apple variables.
unsafe fn extract_applev() -> *mut *mut c_char {
    unsafe extern "C" {
        static mut environ: *mut *mut c_char;
    }

    unsafe {
        let mut envp = environ;
        let mut envc: usize = 0;

        // skip through all envs in envp
        while !(*envp).is_null() {
            envp = envp.add(1);
            envc = envc.saturating_add(1);
        }

        // skip the NULL after envs
        envp = envp.add(1);

        tracing::info!("skipped {} envs from envp", envc);

        envp
    }
}

/// Name of environment variable used to mark whether remote environment has already been fetched.
const REMOTE_ENV_FETCHED: &str = "MIRRORD_REMOTE_ENV_FETCHED";

/// Fetches remote environment from the agent.
/// Uses [`setup`] and [`PROXY_CONNECTION`] globals.
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

#[cfg(all(test, target_os = "macos"))]
mod test {
    use std::ffi::CStr;

    #[test]
    fn test_extract_applev() {
        unsafe {
            let applev = super::extract_applev();
            if applev.is_null() {
                panic!("applev is null");
            } else {
                // first value in applev is exec path
                assert!(
                    CStr::from_ptr(*applev)
                        .to_string_lossy()
                        .contains("executable_path=")
                );
            }
        }
    }
}
