#![cfg(target_os = "windows")]
#![allow(static_mut_refs)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]
#![feature(slice_pattern)]
#![feature(ptr_metadata)]

#[cfg(test)]
mod tests;

mod hooks;
mod macros;
pub mod process;

use std::{io::Write, net::SocketAddr, thread};

use libc::EXIT_FAILURE;
use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::{LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, MIRRORD_LAYER_WAIT_FOR_DEBUGGER};
use mirrord_intproxy_protocol::LayerId;
pub use mirrord_layer_lib::setup::layer_setup;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    proxy_connection::{PROXY_CONNECTION, ProxyConnection},
    setup::init_setup,
};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::{
        libloaderapi::LoadLibraryA,
        winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
    },
};

use crate::hooks::initialize_hooks;
pub static mut DETOUR_GUARD: Option<DetourGuard> = None;

pub const MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID: &str = "MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID";
pub const MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID: &str = "MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID";
pub const MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR: &str = "MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR";
pub const MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64: &str =
    "MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64";

fn initialize_detour_guard() -> anyhow::Result<()> {
    unsafe {
        DETOUR_GUARD = Some(DetourGuard::new()?);
    }

    Ok(())
}

fn release_detour_guard() -> anyhow::Result<()> {
    unsafe {
        // This will release the hooking engine, removing all hooks.
        if let Some(guard) = DETOUR_GUARD.as_mut() {
            guard.try_close()?;
        }
    }

    Ok(())
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
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .with_thread_ids(true)
                    .with_writer(std::io::stderr)
                    .with_file(true)
                    .with_line_number(true)
                    .pretty(),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }
}

fn initialize_windows_proxy_connection() -> LayerResult<()> {
    init_tracing();

    let current_pid = std::process::id();
    if let Ok(Ok(parent_pid)) =
        std::env::var(MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID).map(|x| x.parse::<u32>())
    {
        // Create Windows-specific process info
        let process_info = mirrord_intproxy_protocol::ProcessInfo {
            pid: current_pid as _,
            parent_pid: parent_pid as _,
            name: std::env::current_exe()
                .ok()
                .and_then(|path| path.file_name()?.to_str().map(String::from))
                .unwrap_or_else(|| "unknown".to_string()),
            cmdline: std::env::args().collect(),
            loaded: true,
        };

        // Try to parse `SocketAddr` from [`MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR`] environment
        // variable.
        let address = std::env::var(MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR)
            .map_err(LayerError::MissingEnvIntProxyAddr)?
            .parse::<SocketAddr>()
            .map_err(LayerError::MalformedIntProxyAddr)?;

        let parent_layer = std::env::var(MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID)
            .map(|x| x.parse::<u64>())
            .map_err(|_| LayerError::MissingLayerIdEnv)?
            .map_err(|_| LayerError::MalformedLayerIdEnv)?;

        // Set up session request.
        let session = mirrord_intproxy_protocol::NewSessionRequest {
            parent_layer: Some(LayerId(parent_layer)),
            process_info,
        };

        // Use a default timeout of 30 seconds
        let timeout = std::time::Duration::from_secs(30);
        let new_connection = ProxyConnection::new(address, session, timeout)?;
        PROXY_CONNECTION.set(new_connection).map_err(|_| {
            LayerError::GlobalAlreadyInitialized("Proxy connection already initialized")
        })?;

        let config_base64 = std::env::var(MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64)
            .map_err(|_| LayerError::MissingConfigEnv)?;

        // Read and initialize configuration
        let config = LayerConfig::decode(config_base64.as_str()).map_err(LayerError::Config)?;
        
        // Initialize layer setup with the configuration
        init_setup(config, address)?;
    } else {
        // Create Windows-specific process info
        let process_info = mirrord_intproxy_protocol::ProcessInfo {
            pid: current_pid as _,
            // We don't need parent PID for parent process
            parent_pid: 0,
            name: std::env::current_exe()
                .ok()
                .and_then(|path| path.file_name()?.to_str().map(String::from))
                .unwrap_or_else(|| "unknown".to_string()),
            cmdline: std::env::args().collect(),
            loaded: true,
        };

        // Try to parse `SocketAddr` from [`MIRRORD_LAYER_INTPROXY_ADDR`] environment variable.
        let address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
            .map_err(LayerError::MissingEnvIntProxyAddr)?
            .parse::<SocketAddr>()
            .map_err(LayerError::MalformedIntProxyAddr)?;

        // Set up session request.
        let session = mirrord_intproxy_protocol::NewSessionRequest {
            parent_layer: None,
            process_info,
        };

        // Use a default timeout of 30 seconds
        let timeout = std::time::Duration::from_secs(30);
        let new_connection = ProxyConnection::new(address, session, timeout)?;
        PROXY_CONNECTION.set(new_connection).map_err(|_| {
            LayerError::GlobalAlreadyInitialized("Proxy connection already initialized")
        })?;

        // Read and initialize configuration
        let config = mirrord_config::util::read_resolved_config().map_err(LayerError::Config)?;
        
        // Initialize layer setup with the configuration
        init_setup(config, address)?;
    }

    Ok(())
}

/// Function that gets called upon DLL initialization ([`DLL_PROCESS_ATTACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Successful DLL attach initialization.
/// * [`FALSE`] - Failed DLL attach initialization. Right after this, we will receive a
///   [`DLL_PROCESS_DETACH`] notification as long as no exception is thrown.
/// * Anything else - Failure.
fn dll_attach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    if std::env::var(MIRRORD_LAYER_WAIT_FOR_DEBUGGER).is_ok() {
        wait_for_debug!();
    }

    // Avoid running logic in [`DllMain`] to prevent exceptions.
    let _ = thread::spawn(|| {
        use process::{threads, threads::ThreadState};

        // NOTE(gabriela): workaround for proxy connection thread freeze once we freeze all threads.
        //
        // ```cpp
        // Provider = DCATALOG::LoadProvider((DCATALOG *)v15, (struct PROTO_CATALOG_ITEM *)v20);
        // ```
        unsafe {
            LoadLibraryA(c"mswsock.dll".as_ptr());
        }

        // Before doing anything, suspend all other threads.
        // NOTE(gabriela): otherwise, other logic might run before
        // our hooks are applied.
        //
        // NOTE(gabriela): assumptions:
        // 1. This process will be initiated early enough in the process lifetime, that there will
        //    be 0 threads that *should* and *must* stay frozen.
        //
        // 1.1. Likewise, the [`mirrord_start`] operation, although it may create
        //      threads at will, it should not allow for any threads that are meant to be frozen
        //      to continue for long enough that it might come to be problematic with the freeze
        //      logic.
        //
        // 2. There should be no other code intercepting execution in the same matter as mirrord
        //    does, creating threads within the process at this time. It may work, but it has not
        //    been tested.
        threads::change_all_state(ThreadState::Frozen);

        if let Err(e) = mirrord_start() {
            tracing::error!("Failed call to mirrord_start: {e}");
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(EXIT_FAILURE);
        }

        // Unsuspend other threads
        threads::change_all_state(ThreadState::Unfrozen);

        tracing::info!("mirrord-layer-win fully initialized, threads resumed!");
    });

    TRUE
}

/// Function that gets called upon DLL deinitialization ([`DLL_PROCESS_DETACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Successful DLL detach.
/// * Anything else - Failure.
fn dll_detach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    // Release detour guard
    if let Err(e) = release_detour_guard() {
        tracing::error!(
            "Warning: Failed releasing detour guard during DLL detach: {}",
            e
        );
    }

    TRUE
}

/// Function that gets called upon process thread creation ([`DLL_THREAD_ATTACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Successful process thread attach initialization.
/// * Anything else - Failure.
fn thread_attach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    TRUE
}

/// Function that gets called upon process thread exit ([`DLL_THREAD_DETACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Successful process thread detachment.
/// * Anything else - Failure.
fn thread_detach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    TRUE
}

fn mirrord_start() -> anyhow::Result<()> {
    initialize_windows_proxy_connection()?;
    tracing::info!("ProxyConnection initialized");

    initialize_detour_guard()?;
    tracing::info!("DetourGuard initialized");

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    initialize_hooks(guard)?;
    tracing::info!("Hooks initialized");

    Ok(())
}

entry_point!(|module, reason_for_call, reserved| {
    match reason_for_call {
        DLL_PROCESS_ATTACH => dll_attach(module, reserved),
        DLL_PROCESS_DETACH => dll_detach(module, reserved),
        DLL_THREAD_ATTACH => thread_attach(module, reserved),
        DLL_THREAD_DETACH => thread_detach(module, reserved),
        // Invalid reason for call.
        _ => FALSE,
    }
});
