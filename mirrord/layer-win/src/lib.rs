#![allow(static_mut_refs)]
use std::{
    net::SocketAddr,
    sync::{Mutex, OnceLock},
    thread,
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::{MIRRORD_LAYER_INTPROXY_ADDR, MIRRORD_LAYER_WAIT_FOR_DEBUGGER};
use mirrord_layer_lib::ProxyConnection;
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::{
        consoleapi::AllocConsole,
        debugapi::DebugBreak, processthreadsapi::GetCurrentProcessId,
        winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
    },
};

use crate::hooks::initialize_hooks;

mod error;
mod hooks;
mod macros;
pub mod process;

pub static mut DETOUR_GUARD: Option<DetourGuard> = None;

fn initialize_detour_guard() -> anyhow::Result<()> {
    unsafe {
        DETOUR_GUARD = Some(DetourGuard::new()?);
    }

    Ok(())
}

fn release_detour_guard() -> anyhow::Result<()> {
    unsafe {
        // This will release the hooking engine, removing all hooks.
        DETOUR_GUARD.as_mut().unwrap().try_close()?;
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
    // Avoid running logic in [`DllMain`] to prevent exceptions.
    let _ = thread::spawn(|| {
        mirrord_start().expect("Failed call to mirrord_start");
        println!("mirrord-layer-win fully initialized!");
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
    release_detour_guard().expect("Failed releasing detour guard");

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
    // TODO: turn into more structured module that handles console
    unsafe {
        AllocConsole();
    }

    if std::env::var(MIRRORD_LAYER_WAIT_FOR_DEBUGGER).is_ok() {
        println!("Checking for debugger...");
        if wait_for_debug!() {
            unsafe { DebugBreak() };
        }
    }

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
