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
mod logging;
mod macros;
pub mod process;
mod subprocess;
mod trace_only;

use std::{io::Write, thread};

use libc::EXIT_FAILURE;
use minhook_detours_rs::guard::DetourGuard;
pub use mirrord_layer_lib::setup::layer_setup;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    process::windows::{execution::debug::should_wait_for_debugger, sync::LayerInitEvent},
    proxy_connection::PROXY_CONNECTION,
    setup::init_setup,
};
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
};

use crate::{
    hooks::initialize_hooks,
    logging::init_tracing,
    subprocess::{create_proxy_connection, detect_process_context, get_setup_address},
    trace_only::{is_trace_only_mode, modify_config_for_trace_only},
};
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
        if let Some(guard) = DETOUR_GUARD.as_mut() {
            guard.try_close()?;
        }
    }

    Ok(())
}

fn initialize_windows_proxy_connection() -> LayerResult<()> {
    // init_tracing();

    let process_context = detect_process_context()?;
    let connection = create_proxy_connection(&process_context)?;

    PROXY_CONNECTION.set(connection).map_err(|_| {
        LayerError::GlobalAlreadyInitialized("Proxy connection already initialized")
    })?;

    setup_layer_config(&process_context)?;
    Ok(())
}

fn setup_layer_config(context: &subprocess::ProcessContext) -> LayerResult<()> {
    // Read and initialize configuration using the standard method
    let mut config = mirrord_config::util::read_resolved_config().map_err(LayerError::Config)?;

    // Check if we're in trace only mode (no agent)
    if is_trace_only_mode() {
        modify_config_for_trace_only(&mut config);

        // In trace-only mode, we still need to initialize setup but with a dummy proxy address
        // since no actual proxy communication will occur
        let dummy_proxy_address = "127.0.0.1:0".parse().unwrap();
        init_setup(config, dummy_proxy_address)?;
    } else {
        // Normal mode - get the real proxy address for layer setup
        let setup_address = get_setup_address(context)?;
        init_setup(config, setup_address)?;
    }

    Ok(())
}

fn mirrord_start() -> anyhow::Result<()> {
    // Create layer initialization event first
    let init_event = LayerInitEvent::for_child()
        .map_err(|e| anyhow::anyhow!("Failed to create layer initialization event: {}", e))?;

    // Check for trace-only mode and initialize accordingly
    if is_trace_only_mode() {
        tracing::info!("Running in trace-only mode - skipping proxy connection");

        // In trace-only mode, we still need to set up configuration but skip proxy connection
        let process_context = detect_process_context()
            .map_err(|e| anyhow::anyhow!("Failed to detect process context: {}", e))?;

        setup_layer_config(&process_context)
            .map_err(|e| anyhow::anyhow!("Failed to setup layer config: {}", e))?;

        tracing::info!("Configuration setup completed in trace-only mode");
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
    init_event
        .signal_complete()
        .map_err(|e| anyhow::anyhow!("Failed to signal initialization complete: {}", e))?;

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

        if let Err(e) = mirrord_start() {
            tracing::error!("Failed call to mirrord_start: {e}");
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
