#![cfg(target_os = "windows")]
#![allow(static_mut_refs)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]

#[cfg(test)]
mod tests;

mod diagnostics;
mod hooks;
mod iocp;
mod macros;
mod managed;
pub mod process;
mod subprocess;
mod task_pool;

use std::thread;

use libc::EXIT_FAILURE;
use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::util::read_resolved_config;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    logging::{init_console_logger, init_tracing_sinks},
    process::windows::{
        execution::debug::should_wait_for_debugger, injection::MIRRORD_INJECTION_METHOD_ENV,
        sync::LayerInitEvent,
    },
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

/// Synchronous part of layer startup, run inside [`dll_attach`].
///
/// Everything here is loader-lock-safe (env parsing, `GetProcAddress` on already
/// imported modules, in-process detour patches) and MUST complete before `DllMain`
/// returns: the loader then continues into the target's `main`, so any work left for
/// later would race application code.
///
/// Every injection method already loads the layer before the target's first instruction
/// (APC at thread start, IAT during import resolution, remote thread while the main
/// thread is suspended, debugger early stop for attach). What running here adds is the
/// removal of a second race: once `DllMain` returns, the loader goes straight into the
/// entry point, so a spawned worker has no guarantee of being scheduled first. Doing the
/// install here is what makes the ordering certain rather than usually-wins.
///
/// # Loader lock
///
/// Nothing here may load a module, wait for a thread, or call `eprintln!`. See the warning
/// at the top of `layer-lib`'s `logging` for why, and `init_tracing_sinks` for the split
/// that keeps the console logger's socket off this path.
fn initialize_layer_sync() -> LayerResult<()> {
    init_tracing_sinks();

    // Which injection method brought this layer in (the launching CLI sets it on the
    // child environment). Logged first so every layer log identifies its load path.
    tracing::info!(
        injection_method = std::env::var(MIRRORD_INJECTION_METHOD_ENV)
            .as_deref()
            .unwrap_or("unset"),
        "layer loading"
    );

    let config = read_resolved_config().map_err(LayerError::Config)?;
    init_layer_setup(config, false);

    initialize_detour_guard()?;
    tracing::info!("DetourGuard initialized");

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    initialize_hooks(guard)?;
    tracing::info!("Hooks initialized");

    Ok(())
}

/// Asynchronous part of layer startup, on a worker thread spawned by [`dll_attach`].
///
/// Network-bound and monitor work that must not hold the loader lock. The whole body
/// runs under the internal-thread marker so its own socket/file traffic bypasses the
/// (already live) hooks instead of recursing through the not-yet-established proxy
/// connection.
fn initialize_layer_async() -> LayerResult<()> {
    let _internal = hooks::internal_thread::InternalGuard::enter();

    // Opens a socket, so it cannot run while `DllMain` holds the loader lock.
    init_console_logger();

    // Walks every process on the machine, enumerates the loader's module list, and opens each
    // loaded module on disk to read its version resource. Both helpers it calls document that
    // they must run "at a safe time"; under the loader lock is not one.
    diagnostics::log_early_snapshot();

    let init_event = LayerInitEvent::for_child()?;

    diagnostics::install_crash_handler();

    if is_trace_only_mode() {
        tracing::info!("Running in trace-only mode - skipping proxy connection initialization");
    } else {
        // Normal mode - initialize proxy connection
        initialize_windows_proxy_connection()?;
        tracing::info!("ProxyConnection initialized");
    }

    // Signal that initialization is complete.
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

    // Install everything the target must not be able to run ahead of - all hook
    // families - before returning to the loader. Failing that, run the process
    // without mirrord rather than half-initialized.
    if let Err(error) = initialize_layer_sync() {
        tracing::error!("Synchronous layer initialization failed: {error}");
        return FALSE;
    }

    // The rest (crash monitor registration, proxy connection, ready signal) is
    // network-bound; keep it off the loader lock. Mirrord's own traffic bypasses
    // the hooks through the internal-thread marker; the target's early calls hit
    // the hooks and wait for the proxy connection to come up.
    // Rust claims this thread's handle slot on entry and aborts the process when a hook claimed
    // it first, so keep hooks off Rust thread state - see the warning in `layer-lib::logging`.
    let _ = thread::spawn(move || {
        if let Err(e) = initialize_layer_async() {
            let reason = e.to_string();
            tracing::error!("Failed call to layer_start: {reason}");
            // Tell the monitor this is an init failure before exiting; otherwise the exit runs
            // `DLL_PROCESS_DETACH`, signals a clean shutdown, and the failure is lost.
            diagnostics::signal_init_failure(&reason);
            // Nothing to flush: the layer's sinks are an unbuffered `File` and a raw
            // `WriteFile` to the standard error handle. `std::io::stdout`/`stderr` would only
            // take the reentrant lock this layer must never take.
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
fn dll_detach(_module: HINSTANCE, reserved: LPVOID) -> BOOL {
    // On `DLL_PROCESS_DETACH`, a non-null `lpReserved` means the process is terminating; null means
    // a `FreeLibrary` unload. Only a real termination is a clean shutdown.
    let process_terminating = !reserved.is_null();

    // Unregister the crash handler while the DLL is still mapped.
    diagnostics::uninstall_crash_handler(process_terminating);

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
    // `SetThreadStackGuarantee` is per-thread, so reserve handler stack on every new thread.
    diagnostics::reserve_handler_stack();
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

/// Import-table injection resolves this marker through ordinal 1.
#[unsafe(no_mangle)]
pub extern "system" fn mirrord_stork_marker() {}
