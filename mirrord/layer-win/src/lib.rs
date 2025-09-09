#![allow(static_mut_refs)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]
use std::{
    sync::Mutex,
    thread::{self, JoinHandle},
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_config::MIRRORD_LAYER_WAIT_FOR_DEBUGGER;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};
use winapi::{
    shared::minwindef::{BOOL, FALSE, HINSTANCE, LPVOID, TRUE},
    um::{
        debugapi::DebugBreak,
        processthreadsapi::GetCurrentProcessId,
        winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, DLL_THREAD_ATTACH, DLL_THREAD_DETACH},
    },
};

use crate::hooks::initialize_hooks;

mod console;
mod hooks;
mod macros;
pub mod process;
mod socket;

pub static mut DETOUR_GUARD: Option<DetourGuard> = None;
static INIT_THREAD_HANDLE: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

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
    };
}

fn initialize_windows_proxy_connection() -> anyhow::Result<()> {
    init_tracing();

    // Create Windows-specific process info
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

    // Use the layer-lib function which handles everything internally
    mirrord_layer_lib::common::initialize_proxy_connection(process_info)
        .map_err(|e| anyhow::anyhow!("Failed to initialize proxy connection: {}", e))?;

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
    // Release detour guard - use unwrap_or to avoid panicking during detach
    if let Err(e) = release_detour_guard() {
        eprintln!(
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
    // Create Windows console, and redirects std handles.
    if let Err(e) = console::create() {
        println!("WARNING: couldn't initialize console: {:?}", e);
    }

    println!("Console initialized");

    initialize_windows_proxy_connection()?;
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
