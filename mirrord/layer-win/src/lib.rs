#![allow(static_mut_refs)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]
#![feature(slice_pattern)]
use std::{
    net::SocketAddr,
    sync::{Mutex, OnceLock},
    thread::{self, JoinHandle},
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::{LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, MIRRORD_LAYER_WAIT_FOR_DEBUGGER};
use mirrord_layer_lib::ProxyConnection;
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::{
        debugapi::DebugBreak,
        processthreadsapi::GetCurrentProcessId,
        wincon::{ATTACH_PARENT_PROCESS, AttachConsole},
        winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
    },
};

use crate::{hooks::initialize_hooks, setup::LayerSetup};

pub(crate) mod common;
mod console;
pub mod error;
mod hooks;
mod macros;
pub mod process;
mod setup;
mod socket;

pub static mut DETOUR_GUARD: Option<DetourGuard> = None;
static INIT_THREAD_HANDLE: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Global configuration for the layer
static CONFIG: OnceLock<LayerConfig> = OnceLock::new();

/// Global setup for the layer (outgoing selector, DNS selector, etc.)
static SETUP: OnceLock<LayerSetup> = OnceLock::new();

/// Get access to the layer configuration
pub fn layer_config() -> &'static LayerConfig {
    CONFIG.get().expect("Layer configuration not initialized")
}

/// Get access to the layer setup
pub fn layer_setup() -> &'static LayerSetup {
    SETUP.get().expect("Layer setup not initialized")
}

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

pub(crate) static mut PROXY_CONNECTION: OnceLock<ProxyConnection> = OnceLock::new();

fn initialize_proxy_connection() -> anyhow::Result<()> {
    // Try to parse `SocketAddr` from [`MIMRRORD_LAYER_INTPROXY_ADDR`] environment variable.
    let address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
        .map_err(error::Error::MissingEnvIntProxyAddr)?
        .parse::<SocketAddr>()
        .map_err(error::Error::MalformedIntProxyAddr)?;

    // Read and initialize configuration
    let config = mirrord_config::util::read_resolved_config()
        .map_err(|e| anyhow::anyhow!("Failed to read mirrord configuration: {}", e))?;

    CONFIG
        .set(config.clone())
        .map_err(|_| anyhow::anyhow!("Configuration already initialized"))?;

    // Initialize layer setup with the configuration
    let setup = LayerSetup::new(&config);
    SETUP
        .set(setup)
        .map_err(|_| anyhow::anyhow!("Setup already initialized"))?;

    // Create session request with Windows-specific process info
    let process_info = mirrord_intproxy_protocol::ProcessInfo {
        pid: unsafe { GetCurrentProcessId() as i32 },
        // We don't need parent PID for Windows layer
        parent_pid: 0,
        name: std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name()?.to_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string()),
        cmdline: std::env::args().collect(),
        loaded: true,
    };

    // Set up session request.
    let session = mirrord_intproxy_protocol::NewSessionRequest {
        parent_layer: None,
        process_info,
    };

    // Use a default timeout of 30 seconds
    let timeout = std::time::Duration::from_secs(30);

    let new_connection = ProxyConnection::new(address, session, timeout)?;
    unsafe {
        PROXY_CONNECTION
            .set(new_connection)
            .expect("Could not initialize PROXY_CONNECTION");
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
        println!("Checking for debugger...");
        wait_for_debug!();
        unsafe { DebugBreak() };
    }

    // Avoid running logic in [`DllMain`] to prevent exceptions.
    let handle = thread::spawn(|| {
        mirrord_start().expect("Failed call to mirrord_start");
        println!("mirrord-layer-win fully initialized!");
    });

    // Store the thread handle for cleanup
    if let Ok(mut thread_handle) = INIT_THREAD_HANDLE.lock() {
        *thread_handle = Some(handle);
    }

    TRUE
}

/// Function that gets called upon DLL deinitialization ([`DLL_PROCESS_DETACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Successful DLL detach.
/// * Anything else - Failure.
fn dll_detach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    // Wait for initialization thread to complete (without spawning new threads)
    if let Ok(mut thread_handle) = INIT_THREAD_HANDLE.lock()
        && let Some(handle) = thread_handle.take()
    {
        // This may block but it's safer than spawning threads during detach
        let _ = handle.join();
    }

    // Release detour guard - use unwrap_or to avoid panicking during detach
    if let Err(e) = release_detour_guard() {
        eprintln!(
            "Warning: Failed releasing detour guard during DLL detach: {}",
            e
        );
    }

    // Note: PROXY_CONNECTION is OnceLock and will be cleaned up automatically
    // when the process exits. The underlying TcpStream will close properly.

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
    // Create Windows console, and redirects std handles.
    if let Err(e) = console::create() {
        println!("WARNING: couldn't initialize console: {:?}", e);
    }

    println!("Console initialized");

    initialize_proxy_connection()?;
    println!("ProxyConnection initialized");

    initialize_detour_guard()?;
    println!("DetourGuard initialized");

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    initialize_hooks(guard)?;
    println!("Hooks initialized");

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

#[cfg(test)]
mod tests;
