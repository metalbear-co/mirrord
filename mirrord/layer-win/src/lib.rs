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
mod subprocess;

use std::{io::Write, thread};

use libc::EXIT_FAILURE;
use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::util::read_resolved_config;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    logging::init_tracing,
    process::windows::{execution::debug::should_wait_for_debugger, sync::LayerInitEvent},
    proxy_connection::PROXY_CONNECTION,
    setup::init_layer_setup,
    trace_only::is_trace_only_mode,
};
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
};

use crate::{
    hooks::initialize_hooks,
    subprocess::{create_proxy_connection, detect_process_context},
};
pub static mut DETOUR_GUARD: Option<DetourGuard> = None;

fn initialize_detour_guard() -> LayerResult<()> {
    unsafe {
        DETOUR_GUARD =
            Some(DetourGuard::new().map_err(|err| LayerError::DetourGuard(err.to_string()))?);
    }

    Ok(())
}

fn release_detour_guard() -> LayerResult<()> {
    unsafe {
        // This will release the hooking engine, removing all hooks.
        if let Some(guard) = DETOUR_GUARD.as_mut() {
            guard
                .try_close()
                .map_err(|err| LayerError::DetourGuard(err.to_string()))?;
        }
    }

    Ok(())
}

fn initialize_windows_proxy_connection() -> LayerResult<()> {
    // init_tracing();

    let process_context = detect_process_context()?;
    let connection = create_proxy_connection(&process_context)?;

    unsafe {
        // SAFETY
        // Called only from library constructor.
        #[allow(static_mut_refs)]
        PROXY_CONNECTION
            .set(connection)
            .expect("setting PROXY_CONNECTION singleton")
    }

    Ok(())
}

fn layer_start() -> LayerResult<()> {
    // Create layer initialization event first
    let init_event = LayerInitEvent::for_child()?;

    let config = read_resolved_config().map_err(LayerError::Config)?;
    init_layer_setup(config, false);

    if is_trace_only_mode() {
        tracing::info!("Running in trace-only mode - skipping proxy connection initialization");
    } else {
        // Normal mode - initialize proxy connection
        initialize_windows_proxy_connection()?;
        tracing::info!("ProxyConnection initialized");
    }

    initialize_detour_guard()?;
    tracing::info!("DetourGuard initialized");

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    initialize_hooks(guard)?;
    tracing::info!("Hooks initialized");

    // Signal that initialization is complete
    init_event.signal_complete()?;

    if is_trace_only_mode() {
        tracing::info!("mirrord-layer-win fully initialized in trace-only mode");
    } else {
        tracing::info!("mirrord-layer-win fully initialized");
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
    if should_wait_for_debugger() {
        wait_for_debug!();
    }

    // Avoid running logic in [`DllMain`] to prevent exceptions.
    let _ = thread::spawn(move || {
        init_tracing();

        if let Err(e) = layer_start() {
            tracing::error!("Failed call to layer_start: {e}");
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(EXIT_FAILURE);
        }
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
