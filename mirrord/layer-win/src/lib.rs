#![cfg(target_os = "windows")]
#![allow(static_mut_refs)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]

#[cfg(test)]
mod tests;

mod hooks;
mod iocp;
mod macros;
mod managed;
pub mod process;
mod subprocess;
mod task_pool;

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
    let process_context = detect_process_context()?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "process_context_detected",
        process_context = ?process_context,
        "detected Windows layer process context"
    );
    let connection = create_proxy_connection(&process_context)?;

    tracing::info!(
        event = "windows_layer_init",
        stage = "proxy_connection_ready",
        layer_id = connection.layer_id().0,
        proxy_addr = %connection.proxy_addr(),
        "connected Windows layer to internal proxy"
    );

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
    tracing::info!(
        event = "windows_layer_init",
        stage = "layer_start_begin",
        pid = std::process::id(),
        "starting Windows layer initialization"
    );

    let init_event = LayerInitEvent::for_child()?;

    tracing::info!(
        event = "windows_layer_init",
        stage = "config_read_begin",
        "reading resolved layer configuration"
    );
    let config = read_resolved_config().map_err(LayerError::Config)?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "config_read_complete",
        "read resolved layer configuration"
    );
    init_layer_setup(config, false);

    if is_trace_only_mode() {
        tracing::info!("Running in trace-only mode - skipping proxy connection initialization");
    } else {
        // Normal mode - initialize proxy connection
        initialize_windows_proxy_connection()?;
        tracing::info!("ProxyConnection initialized");
    }

    initialize_detour_guard()?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "detour_guard_ready",
        "initialized Windows detour guard"
    );

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    tracing::info!(
        event = "windows_layer_init",
        stage = "hooks_initialize_begin",
        "initializing Windows layer hooks"
    );
    initialize_hooks(guard)?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "hooks_initialize_complete",
        "initialized Windows layer hooks"
    );

    // Signal that initialization is complete.
    tracing::info!(
        event = "windows_layer_init",
        stage = "init_event_signal_begin",
        "signaling layer initialization completion"
    );
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
        eprintln!(
            "windows_layer_init stage=dll_init_thread_entered pid={}",
            std::process::id()
        );
        init_tracing();

        tracing::info!(
            event = "windows_layer_init",
            stage = "dll_tracing_ready",
            pid = std::process::id(),
            "initialized Windows layer tracing"
        );

        if let Err(e) = layer_start() {
            tracing::error!(
                event = "windows_layer_init",
                stage = "layer_start_failed",
                error = %e,
                "Windows layer initialization failed"
            );
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
    tracing::info!(
        event = "windows_layer_init",
        stage = "dll_detach_begin",
        pid = std::process::id(),
        "detaching Windows layer DLL"
    );

    // Release detour guard
    if let Err(e) = release_detour_guard() {
        tracing::error!(
            "Warning: Failed releasing detour guard during DLL detach: {}",
            e
        );
    }

    tracing::info!(
        event = "windows_layer_init",
        stage = "dll_detach_complete",
        pid = std::process::id(),
        "detached Windows layer DLL"
    );

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
