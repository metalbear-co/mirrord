#![allow(static_mut_refs)]
use std::{net::SocketAddr, sync::OnceLock, thread};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::MIRRORD_LAYER_INTPROXY_ADDR;
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::{
        consoleapi::AllocConsole,
        winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
    },
};

use crate::{hooks::initialize_hooks, proxy_connection::ProxyConnection};

pub(crate) mod error;
pub(crate) mod hooks;
pub(crate) mod macros;
pub(crate) mod process;
pub(crate) mod proxy_connection;

/// Global variable holding the [`DetourGuard`]. Must be mutable as
/// applying, creating, etc... hooks modifies internal state.
pub(crate) static mut DETOUR_GUARD: Option<DetourGuard> = None;

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
    let address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
        .map_err(error::Error::MissingEnvIntProxyAddr)?
        .parse::<SocketAddr>()
        .map_err(error::Error::MalformedIntProxyAddr)?;

    let new_connection = ProxyConnection::new(address)?;
    dbg!(&new_connection);

    unsafe {
        PROXY_CONNECTION.set(new_connection).expect("Could not initialize PROXY_CONNECTION");
    }

    Ok(())
}

/// Function that gets called upon DLL initialization ([`DLL_PROCESS_ATTACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Succesful DLL attach initialization.
/// * [`FALSE`] - Failed DLL attach initialization. Right after this, we will receive a
///   [`DLL_PROCESS_DETACH`] notification as long as no exception is thrown.
/// * Anything else - Failure.
fn dll_attach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    // Avoid running logic in [`DllMain`] to prevent exceptions.
    let _ = thread::spawn(|| {
        mirrord_start().expect("Failed initializing mirrord-layer-win");
    });

    TRUE
}

/// Function that gets called upon DLL deinitialization ([`DLL_PROCESS_DETACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Succesful DLL deattach.
/// * Anything else - Failure.
fn dll_detach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    release_detour_guard().expect("Failed releasing detour guard");

    TRUE
}

/// Function that gets called upon process thread creation ([`DLL_THREAD_ATTACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Succesful process thread attach initialization.
/// * Anything else - Failure.
fn thread_attach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    TRUE
}

/// Function that gets called upon process thread exit ([`DLL_THREAD_DETACH`]).
///
/// # Return value
///
/// * [`TRUE`] - Succesful process thread deattachment.
/// * Anything else - Failure.
fn thread_detach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    TRUE
}

fn mirrord_start() -> anyhow::Result<()> {
    // TODO: turn into more structured module that handles console
    unsafe {
        AllocConsole();
    }

    initialize_proxy_connection()?;
    initialize_detour_guard()?;

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    initialize_hooks(guard)?;

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
