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
mod ide_only;
mod logging;
mod macros;
mod mode;
pub mod process;
mod subprocess;
mod trace_only;

use std::{io::Write, thread};

use libc::EXIT_FAILURE;
use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::{
    LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
};
pub use mirrord_layer_lib::setup::layer_setup;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    ide::{MIRRORD_IDE_ORCHESTRATED_ENV, is_ide_orchestrated, is_user_process},
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
    ide_only::is_propagating_layer,
    logging::init_tracing,
    mode::ExecutionMode,
    subprocess::{create_proxy_connection, detect_process_context, get_setup_address},
    trace_only::{is_trace_only_mode, modify_config_for_trace_only},
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
    let connection = create_proxy_connection(&process_context)?;

    PROXY_CONNECTION.set(connection).map_err(|_| {
        LayerError::GlobalAlreadyInitialized("Proxy connection already initialized")
    })?;

    setup_layer_config(Some(&process_context))?;
    Ok(())
}

fn setup_layer_config(context: Option<&subprocess::ProcessContext>) -> LayerResult<()> {
    let propagating_layer = is_propagating_layer();
    let trace_only = is_trace_only_mode();

    // Read and initialize configuration (default if we're just propagating layer!)
    let mut config = if propagating_layer {
        // Set up default config
        let mut cfg_context = ConfigContext::default();

        LayerFileConfig::default()
            .generate_config(&mut cfg_context)
            .map_err(LayerError::Config)?
    } else {
        mirrord_config::util::read_resolved_config().map_err(LayerError::Config)?
    };

    let proxy_address = if trace_only {
        modify_config_for_trace_only(&mut config);

        // In trace-only mode, we still need to initialize setup but with a dummy proxy address
        // since no actual proxy communication will occur
        "127.0.0.1:0".parse().unwrap()
    } else if propagating_layer {
        // Ditto
        "127.0.0.1:0".parse().unwrap()
    } else {
        // Normal mode - get the real proxy address for layer setup
        // context is required in this case
        let context = context.ok_or_else(|| {
            LayerError::Config(mirrord_config::config::ConfigError::ValueNotProvided(
                "setup_layer_config",
                "context",
                None,
            ))
        })?;
        get_setup_address(context)?
    };

    let local_hostname = trace_only || !config.feature.hostname;

    init_setup(config, proxy_address, local_hostname)
}

fn mirrord_start() -> LayerResult<()> {
    let init_event = LayerInitEvent::for_child()?;

    // If IDE-orchestrated mode but this IS the user process, remove the envvar
    // so child processes spawned by user code don't inherit this flag
    if is_ide_orchestrated() && is_user_process() {
        unsafe { std::env::remove_var(MIRRORD_IDE_ORCHESTRATED_ENV) };
    }

    // Set up configuration and proxy connection based on mode
    let mode = ExecutionMode::detect();
    match mode {
        ExecutionMode::Ide => {
            setup_layer_config(None)?;
        }
        ExecutionMode::TraceOnly => {
            let process_context = detect_process_context()?;
            setup_layer_config(Some(&process_context))?;
        }
        ExecutionMode::Normal => {
            initialize_windows_proxy_connection()?;
            tracing::info!("ProxyConnection initialized");
        }
    }

    initialize_detour_guard()?;
    tracing::info!("DetourGuard initialized");

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    initialize_hooks(guard)?;
    tracing::info!("Hooks initialized");

    init_event.signal_complete()?;

    tracing::info!("mirrord-layer-win initialized in {mode} mode");

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
