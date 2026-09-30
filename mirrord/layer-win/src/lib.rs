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

use std::{any::Any, error::Error, panic::AssertUnwindSafe, thread};

use libc::EXIT_FAILURE;
use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::util::read_resolved_config;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    logging::{init_console_logger, init_tracing_sinks},
    process::windows::{
        execution::debug::should_wait_for_debugger,
        injection::MIRRORD_INJECTION_METHOD_ENV,
        sync::{ChildInitEvent, signal_init_failure_to_parent},
    },
    proxy_connection::{abandon_proxy_connection, install_proxy_connection},
    setup::try_init_layer_setup,
    trace_only::is_trace_only_mode,
};
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::{
        processthreadsapi::{GetCurrentProcess, TerminateProcess},
        winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
    },
};

use crate::{
    hooks::initialize_hooks,
    subprocess::{create_proxy_connection, detect_process_context},
};

/// The hook engine, for as long as the layer is loaded.
///
/// A global rather than a value `dll_attach` owns, because `DLL_PROCESS_DETACH` releases it from a
/// separate call. It is also never dropped on a failed start: its `Drop` reports a failure with
/// `eprintln!`, which must not run under the loader lock (see `layer-lib::logging`), so the failure
/// path takes it out and forgets it instead.
pub static mut DETOUR_GUARD: Option<DetourGuard> = None;

fn initialize_detour_guard() -> LayerResult<&'static mut DetourGuard<'static>> {
    let guard = DetourGuard::new().map_err(|err| LayerError::DetourGuard(err.to_string()))?;
    // SAFETY: only `DllMain` writes the guard, under the loader lock.
    Ok(unsafe { DETOUR_GUARD.insert(guard) })
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

    install_proxy_connection(connection)
}

/// The text of a caught panic's payload.
pub(crate) fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic>")
}

/// Logs which injection method brought this layer in (the launching CLI sets it on the child
/// environment), first, so every layer log identifies its load path.
fn record_injection_method() {
    tracing::info!(
        injection_method = std::env::var(MIRRORD_INJECTION_METHOD_ENV)
            .as_deref()
            .unwrap_or("unset"),
        "layer loading"
    );
}

/// Synchronous part of layer startup, run inside [`dll_attach`].
///
/// Everything here is loader-lock-safe (env parsing, `GetProcAddress` on already imported
/// modules, in-process detour patches) and must complete before `DllMain` returns: work left
/// for a thread of our own would race whatever the target runs next.
///
/// # What "before the target runs" means
///
/// For a process mirrord launches, every injection method loads the layer before the
/// executable's entry point: load-library through a remote thread while the primary thread is
/// still suspended, APC when the primary thread first runs, IAT while the loader resolves the
/// executable's imports. Installing the hooks here, rather than on a worker, is what makes them
/// live before that entry point, instead of usually winning a race with it.
///
/// It is not "before any code of the target". The DLLs the executable imports run their own
/// initializers (`DllMain`, C++ static constructors, TLS callbacks) during process
/// initialization, and depending on the method and the import order some of them run before this
/// layer, so a socket or file one of them opens there is not intercepted.
///
/// Attaching to a running process gives no ordering guarantee at all: the target has been
/// running for as long as it has, and only an APC attach at an early debugger stop comes close.
///
/// # Loader lock
///
/// Nothing here may load a module, wait for a thread, spawn one, or call `eprintln!`. See the
/// warning at the top of `layer-lib`'s `logging` for why, and `init_tracing_sinks` for the split
/// that keeps the console logger's socket off this path.
///
/// # Returns
///
/// The parent's readiness event, claimed for the startup worker to signal, or `None` when no
/// parent waits, which is normal for a process mirrord did not create.
///
/// # Errors
///
/// Any error leaves every hook disabled; see [`hooks::initialize_hooks`].
fn initialize_layer_sync() -> Result<Option<ChildInitEvent>, Box<dyn Error>> {
    init_tracing_sinks();

    record_injection_method();

    diagnostics::log_loader_snapshot();

    // Trace-only mode never connects, so a hooked call must not wait for the connection.
    if is_trace_only_mode() {
        abandon_proxy_connection();
    }

    // Claim the parent's readiness event here, not on the worker thread.
    //
    // The parent creates this event immediately before it injects this layer, and drops it as
    // soon as its wait ends. A worker thread that opens it later races that drop, and a process
    // that loses the race finds no event although one existed when it was injected. Holding a
    // handle from here keeps the name alive for as long as this layer needs it.
    let ready = ChildInitEvent::open()
        // Not a reason to stop. Nothing is waiting for a signal that nobody will read, and the
        // layer has everything else it needs from the environment.
        .inspect_err(|error| {
            tracing::warn!(
                %error,
                "no parent is waiting for this layer, so it reports no readiness"
            )
        })
        .ok();

    let config = read_resolved_config().map_err(LayerError::Config)?;
    try_init_layer_setup(config, false)?;

    // The queue exists before any hook can submit to it. Its workers start on the startup
    // worker, so a failure below leaves no thread of ours behind.
    task_pool::prepare();

    let guard = initialize_detour_guard()?;
    tracing::info!("DetourGuard initialized");

    initialize_hooks(guard)?;
    tracing::info!("Hooks initialized");

    Ok(ready)
}

/// Asynchronous part of layer startup, on a worker thread spawned by [`dll_attach`].
///
/// Network-bound and monitor work that must not hold the loader lock, in the order the target
/// needs it: the crash monitor registration (so a short-lived process that crashes is still
/// covered), the proxy connection, the readiness signal, and only then the diagnostics nothing in
/// the target waits for.
///
/// The caller runs it under the internal-thread marker, so its own socket/file traffic bypasses
/// the (already live) hooks instead of recursing through the not-yet-established proxy
/// connection.
///
/// # Arguments
///
/// * `ready` - the readiness event [`initialize_layer_sync`] claimed, when a parent waits.
fn initialize_layer_async(ready: Option<ChildInitEvent>) -> LayerResult<()> {
    // Opens a socket, so it cannot run while `DllMain` holds the loader lock.
    init_console_logger();

    #[cfg(debug_assertions)]
    fault_injection::panic_before_registration();

    // Jobs submitted by hooks since `DllMain` wait in the queue until now.
    task_pool::start_workers()?;

    diagnostics::register_crash_monitor();

    let trace_only = is_trace_only_mode();
    if trace_only {
        tracing::info!("Running in trace-only mode - skipping proxy connection initialization");
    } else {
        initialize_windows_proxy_connection()?;
        tracing::info!("ProxyConnection initialized");
    }

    if let Some(ready) = ready {
        ready.signal_complete()?;
    }

    diagnostics::log_process_context();

    if trace_only {
        tracing::info!("mirrord-layer-win fully initialized in trace-only mode");
    } else {
        tracing::info!("mirrord-layer-win fully initialized");
    }

    Ok(())
}

/// Runs the asynchronous startup, and ends the process when it fails or panics.
///
/// A panic is a failure like any other: the hooks are already live, so a startup that stopped
/// half-way would leave every remote call waiting for a connection that never comes.
fn run_startup_worker(ready: Option<ChildInitEvent>) {
    let _internal = hooks::internal_thread::InternalGuard::enter();

    let started = std::panic::catch_unwind(AssertUnwindSafe(|| initialize_layer_async(ready)))
        .map_err(|panic| format!("panicked: {}", panic_message(&*panic)))
        .and_then(|started| started.map_err(|error| error.to_string()));
    if let Err(reason) = started {
        fail_startup(&reason);
    }
}

/// Ends a process whose layer's asynchronous startup failed.
///
/// The process cannot run correctly: its hooks are live, and the connection they need will
/// never exist.
fn fail_startup(reason: &str) -> ! {
    // First, before anything that can block: hooked calls waiting for the proxy connection give
    // up now rather than at their timeout.
    abandon_proxy_connection();
    tracing::error!("the layer's startup failed, so the process ends: {reason}");
    // A waiting parent stops waiting at once, rather than at its timeout.
    signal_init_failure_to_parent();
    // Tell the monitor this is an init failure before exiting; otherwise the exit runs
    // `DLL_PROCESS_DETACH`, signals a clean shutdown, and the failure is lost.
    diagnostics::signal_init_failure(reason);
    // Nothing to flush: the layer's sinks are an unbuffered `File` and a raw `WriteFile` to the
    // standard error handle. `std::io::stdout`/`stderr` would only take the reentrant lock this
    // layer must never take.
    std::process::exit(EXIT_FAILURE);
}

/// Puts a layer whose startup worker could not be started back into the state of a failed
/// synchronous startup: loaded, but inert, so the process can run without mirrord.
///
/// Runs inside `DllMain`. Disabling the hooks patches the same functions, under the same thread
/// freeze, that enabling them did a moment earlier in the same `DllMain`, so it is as safe under
/// the loader lock. The hook engine itself stays initialized until `DLL_PROCESS_DETACH`, unlike
/// after a failed synchronous startup: there the hooks were never enabled, while here another
/// thread may already be inside a detour, and it still calls the original function through the
/// engine's trampoline.
///
/// A process whose hooks cannot be disabled has no way to run correctly, with or without mirrord,
/// so it is ended.
fn startup_worker_not_started() {
    // Every call already waiting for the connection is released.
    abandon_proxy_connection();

    let disabled = unsafe { DETOUR_GUARD.as_mut() }.map_or(Ok(()), DetourGuard::disable_all_hooks);
    if let Err(error) = disabled {
        tracing::error!(
            %error,
            "failed to disable the hooks of a layer that cannot start, so the process ends"
        );
        // The parent reports it at once, rather than waiting for the exit.
        signal_init_failure_to_parent();
        // `TerminateProcess` rather than `exit`: it runs no other module's detach under the
        // loader lock this thread holds.
        unsafe { TerminateProcess(GetCurrentProcess(), EXIT_FAILURE as u32) };
    }
    // The `SetUnhandledExceptionFilter` hook is off, so the target's filter owns the slot again.
    utils_win::diagnostics::crash::restore_filter();

    // The parent reports it at once, rather than at its timeout.
    signal_init_failure_to_parent();
}

/// Development-build switches that make the startup fail on purpose, so the paths that handle a
/// failure can be exercised end to end.
#[cfg(debug_assertions)]
mod fault_injection {
    /// Set to `1` to make the asynchronous startup panic before it registers with the crash
    /// monitor.
    const PANIC_BEFORE_REGISTRATION: &str = "MIRRORD_LAYER_DEBUG_PANIC_BEFORE_REGISTRATION";

    pub(super) fn panic_before_registration() {
        if std::env::var(PANIC_BEFORE_REGISTRATION).is_ok_and(|value| value == "1") {
            panic!("{PANIC_BEFORE_REGISTRATION} is set");
        }
    }
}

/// Function that gets called upon DLL initialization ([`DLL_PROCESS_ATTACH`]).
///
/// # Return value
///
/// Always [`TRUE`]. A layer whose synchronous startup failed, or whose startup worker could not be
/// started, stays loaded but inert: no hook enabled, no thread started, the parent told through
/// the failure event. Returning [`FALSE`] instead would fail an import-table load with
/// `STATUS_DLL_INIT_FAILED`, so the target would not start at all, and would hide the module from
/// a load-library injector, which then cannot tell a failed layer from a failed load.
fn dll_attach(_module: HINSTANCE, _reserved: LPVOID) -> BOOL {
    if should_wait_for_debugger() {
        wait_for_debug!();
    }

    // Install everything the target must not be able to run ahead of - all hook
    // families - before returning to the loader. Failing that, run the process
    // without mirrord rather than half-initialized. A panic must not leave `DllMain`
    // either: Rust would end the target process rather than unwind into the loader.
    let installed = std::panic::catch_unwind(AssertUnwindSafe(initialize_layer_sync))
        .unwrap_or_else(|panic| Err(format!("panicked: {}", panic_message(&*panic)).into()));
    let ready = match installed {
        Ok(ready) => ready,
        Err(error) => {
            tracing::error!("Synchronous layer initialization failed: {error}");
            // The hooks were only created, never enabled (see `hooks::initialize_hooks`);
            // releasing the engine frees them without touching any target function.
            if let Some(mut guard) = unsafe { DETOUR_GUARD.take() } {
                if let Err(error) = guard.try_close() {
                    tracing::warn!(%error, "failed to release the hook engine after a failed start");
                }
                // Its `Drop` would close the engine a second time, and report that with
                // `eprintln!`.
                std::mem::forget(guard);
            }
            // The parent turns this into a crash report. Nothing richer is possible from here:
            // the crash monitor is reached by opening a socket, which must not happen under the
            // loader lock, and no thread of ours is started for a layer that failed. Setting a
            // named event needs no module load.
            signal_init_failure_to_parent();
            return TRUE;
        }
    };

    // The rest (crash monitor registration, proxy connection, ready signal) is network-bound;
    // keep it off the loader lock. The target's early calls hit the hooks and wait for the proxy
    // connection to come up.
    // See the warning in `layer-lib::logging`.
    if let Err(error) = thread::Builder::new().spawn(move || run_startup_worker(ready)) {
        tracing::error!(%error, "failed to start the layer's startup worker");
        startup_worker_not_started();
    }

    TRUE
}

// Function that gets called upon DLL deinitialization ([`DLL_PROCESS_DETACH`]).
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
