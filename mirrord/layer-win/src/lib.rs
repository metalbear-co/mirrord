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

use crate::hooks::initialize_hooks;
use mirrord_layer_lib::ProxyConnection;

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
    let address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
        .map_err(error::Error::MissingEnvIntProxyAddr)?
        .parse::<SocketAddr>()
        .map_err(error::Error::MalformedIntProxyAddr)?;

    // Create session request with Windows-specific process info
    let process_info = mirrord_intproxy_protocol::ProcessInfo {
        pid: unsafe { winapi::um::processthreadsapi::GetCurrentProcessId() as i32 },
        parent_pid: 0, // We don't need parent PID for Windows layer
        name: std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name()?.to_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string()),
        cmdline: std::env::args().collect(),
        loaded: true,
    };
    
    let session = mirrord_intproxy_protocol::NewSessionRequest {
        parent_layer: None,
        process_info,
    };
    
    // Use a default timeout of 30 seconds
    let timeout = std::time::Duration::from_secs(30);

    let new_connection = ProxyConnection::new(address, session, timeout)
        .map_err(|e| anyhow::anyhow!("Failed to create proxy connection: {:?}", e))?;

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
    
    // Temporarily disable debugger wait to test if hooks work
    // // Wait for debugger to attach
    // {
    //     use winapi::um::debugapi::IsDebuggerPresent;
    //     // Busy loop until debugger is attached
    //     while unsafe { IsDebuggerPresent() } == 0 {
    //         // Busy wait - no sleep to ensure immediate detection
    //     }
    //     // Trigger a breakpoint when debugger is attached
    //     unsafe {
    //         winapi::um::debugapi::DebugBreak();
    //     }
    // }
    
    // Avoid running logic in [`DllMain`] to prevent exceptions.
    let _ = thread::spawn(|| {
        match mirrord_start() {
            Ok(()) => {
                println!("✅ mirrord-layer-win fully initialized!");
            }
            Err(e) => {
                println!("❌ mirrord-layer-win initialization failed: {}", e);
                println!("❌ Error details: {:?}", e);
            }
        }
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
    println!("🚀 Starting mirrord-layer-win initialization...");
    
    initialize_proxy_connection()?;
    println!("✅ ProxyConnection initialized");
    
    initialize_detour_guard()?;
    println!("✅ DetourGuard initialized");

    // Initialize the Windows setup system
    // setup::initialize_setup()?;
    // println!("✅ Windows setup initialized");

    // // TODO: turn into more structured module that handles console
    unsafe {
        AllocConsole();
    }
    println!("✅ Console allocated");

    let guard = unsafe { DETOUR_GUARD.as_mut().unwrap() };
    println!("🔧 About to initialize hooks...");
    
    match initialize_hooks(guard) {
        Ok(()) => {
            println!("✅ All hooks initialized successfully!");
        }
        Err(e) => {
            println!("❌ Hook initialization failed: {}", e);
            println!("❌ Error details: {:?}", e);
            return Err(e);
        }
    }

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
