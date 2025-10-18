//! Process creation operations for Windows hooks.
//!
//! This module contains the core business logic for creating processes with mirrord
//! layer injection, separated from the low-level hook infrastructure.

use std::{sync::OnceLock, ptr};

use mirrord_layer_lib::process::windows::execution::{
    command::ProcessExecutor,
    CreateProcessInternalWType,
    pipes::{PipeConfig, forward_pipe_raw_handle_threaded},
};
use str_win::{lpcwstr_to_string, lpcwstr_to_string_or_empty};
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD},
        ntdef::{HANDLE, LPCWSTR, LPWSTR, NULL},
    },
    um::{
        handleapi::{CloseHandle, SetHandleInformation},
        namedpipeapi::CreatePipe,
        processenv::GetStdHandle,
        processthreadsapi::{LPSTARTUPINFOW, PROCESS_INFORMATION},
        winbase::{HANDLE_FLAG_INHERIT, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
    },
};

/// Static storage for the original CreateProcessInternalW function pointer.
pub static CREATE_PROCESS_INTERNAL_W_ORIGINAL: OnceLock<&CreateProcessInternalWType> = OnceLock::new();

/// Create a process with mirrord layer injection using the unified executor.
pub fn create_process_with_layer_injection(
    application_name: LPCWSTR,
    command_line: LPWSTR,
    current_directory: LPCWSTR,
    _startup_info: LPSTARTUPINFOW,
    _creation_flags: DWORD,
    _inherit_handles: BOOL,
) -> Result<PROCESS_INFORMATION, String> {
    // Convert Windows API parameters to Rust types
    let app_name = unsafe { lpcwstr_to_string(application_name) };
    let cmd_line = unsafe { lpcwstr_to_string_or_empty(command_line) };
    let current_dir = unsafe { lpcwstr_to_string(current_directory) };

    // Create layer hook executor with auto-configuration
    let mut command = ProcessExecutor::new_layer_hook(&cmd_line)
        .map_err(|e| format!("Failed to create layer hook executor: {}", e))?;

    if let Some(app_name) = app_name {
        command = command.arg(&app_name);
    }
    if let Some(current_dir) = current_dir {
        command = command.current_dir(&current_dir);
    }

    // Create stdio pipes if needed
    let pipes = create_stdio_pipes()?;
    
    // Execute process
    let result = command.spawn()
        .map_err(|e| format!("Process execution failed: {}", e))?;

    // Setup pipe forwarding if pipes were created
    if let Some((stdout_read, stdout_write, stderr_read, stderr_write)) = pipes {
        setup_pipe_forwarding(stdout_read, stdout_write, stderr_read, stderr_write);
    }

    // Return Windows API-compatible process information
    Ok(PROCESS_INFORMATION {
        hProcess: result.process_handle,
        hThread: ptr::null_mut(),
        dwProcessId: result.process_id,
        dwThreadId: result.thread_id,
    })
}

/// Create pipes for stdout/stderr redirection.
fn create_stdio_pipes() -> Result<Option<(HANDLE, HANDLE, HANDLE, HANDLE)>, String> {
    let mut stdout_read: HANDLE = NULL;
    let mut stdout_write: HANDLE = NULL;
    let mut stderr_read: HANDLE = NULL;
    let mut stderr_write: HANDLE = NULL;

    let pipes_created = unsafe {
        CreatePipe(&mut stdout_read, &mut stdout_write, ptr::null_mut(), 0) != 0
            && CreatePipe(&mut stderr_read, &mut stderr_write, ptr::null_mut(), 0) != 0
            && SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT, 0) != 0
            && SetHandleInformation(stderr_read, HANDLE_FLAG_INHERIT, 0) != 0
    };

    if pipes_created {
        Ok(Some((stdout_read, stdout_write, stderr_read, stderr_write)))
    } else {
        Ok(None)
    }
}

/// Setup stdio pipe forwarding in background threads.
fn setup_pipe_forwarding(stdout_read: HANDLE, stdout_write: HANDLE, stderr_read: HANDLE, stderr_write: HANDLE) {
    unsafe {
        CloseHandle(stdout_write);
        CloseHandle(stderr_write);

        let parent_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let parent_stderr = GetStdHandle(STD_ERROR_HANDLE);

        // Start background pipe forwarding threads
        let pipe_config = PipeConfig::default();
        forward_pipe_raw_handle_threaded(
            stdout_read as usize,
            parent_stdout as usize,
            pipe_config.clone(),
        );
        forward_pipe_raw_handle_threaded(
            stderr_read as usize,
            parent_stderr as usize,
            pipe_config,
        );
    }
}