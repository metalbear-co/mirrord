//! Windows process creation hooks module.
//!
//! This module provides hooks for intercepting Windows process creation
//! and injecting the mirrord layer DLL into child processes for transparent
//! system call interception.

use std::sync::OnceLock;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    error::{LayerError, windows::WindowsError},
    process::windows::execution::{CreateProcessInternalWType, LayerManagedProcess},
};
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID, TRUE},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
    },
    um::{
        minwinbase::LPSECURITY_ATTRIBUTES,
        processthreadsapi::{
            LPPROCESS_INFORMATION, LPSTARTUPINFOW, PROCESS_INFORMATION, STARTUPINFOW,
        },
        winnt::PHANDLE,
    },
};

use crate::{apply_hook, process::parse_environment_block, hooks::socket::utils::ERROR_SUCCESS_I32};

/// Static storage for the original CreateProcessInternalW function pointer.
static CREATE_PROCESS_INTERNAL_W_ORIGINAL: OnceLock<&CreateProcessInternalWType> = OnceLock::new();

/// Windows API hook for CreateProcessInternalW function.
///
/// This function intercepts calls to the internal Windows process creation API
/// and redirects them through our unified mirrord process creation system.
/// Falls back to the original implementation if unified creation fails.
unsafe extern "system" fn create_process_internal_w_hook(
    user_token: HANDLE,
    application_name: LPCWSTR,
    command_line: LPWSTR,
    process_attributes: LPSECURITY_ATTRIBUTES,
    thread_attributes: LPSECURITY_ATTRIBUTES,
    inherit_handles: BOOL,
    creation_flags: DWORD,
    environment: LPVOID,
    current_directory: LPCWSTR,
    startup_info: LPSTARTUPINFOW,
    process_information: LPPROCESS_INFORMATION,
    restricted_user_token: PHANDLE,
) -> BOOL {
    tracing::debug!("🎯 CreateProcessInternalW hook intercepted process creation");

    // // winapi loop until IsDebuggerPresent is false
    // use winapi::um::debugapi::{IsDebuggerPresent, DebugBreak};
    // while IsDebuggerPresent() == 0 {
    //     std::thread::sleep(std::time::Duration::from_millis(10));
    // }

    // Convert Windows API parameters to Rust types
    let app_name = unsafe { str_win::lpcwstr_to_string(application_name) };
    let cmd_line = unsafe { str_win::lpcwstr_to_string_or_empty(command_line) };
    let current_dir = unsafe { str_win::lpcwstr_to_string(current_directory) };

    // Get the original function pointer
    let original = match CREATE_PROCESS_INTERNAL_W_ORIGINAL.get() {
        Some(original_fn) => original_fn,
        None => {
            tracing::error!(
                "❌ Original CreateProcessInternalW function not available - hook not properly installed"
            );
            return winapi::shared::minwindef::FALSE;
        }
    };

    // Parse environment from Windows API call
    let env_vars = unsafe { parse_environment_block(environment as *mut std::ffi::c_void) };

    tracing::debug!(
        "Windows CreateProcess parameters: app_name={:?}, cmd_line={:?}, current_dir={:?}, parent_pid={}, env_count={}",
        app_name,
        cmd_line,
        current_dir,
        std::process::id(),
        env_vars.len()
    );

    // Execute process using closure to preserve all original parameters
    // This ensures perfect fidelity to the original CreateProcessInternalW call
    let create_process_fn = |adjusted_creation_flags,
                             adjusted_environment,
                             adjusted_startup_info: &mut STARTUPINFOW| {
        // Create the process suspended with all original and processed parameters
        let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let success = unsafe {
            original(
                user_token,
                application_name,
                command_line,
                process_attributes,
                thread_attributes,
                inherit_handles,
                adjusted_creation_flags,
                adjusted_environment,
                current_directory,
                adjusted_startup_info,
                &mut process_info,
                restricted_user_token,
            )
        };

        if success != ERROR_SUCCESS_I32 {
            Ok(process_info)
        } else {
            Err(LayerError::WindowsProcessCreation(
                WindowsError::last_error(),
            ))
        }
    };

    match LayerManagedProcess::execute_with_closure(
        env_vars,
        creation_flags,
        unsafe { &mut *startup_info },
        create_process_fn,
    )
    .map(|managed_process| managed_process.release())
    {
        Ok(proc_info) => {
            // Success: populate output parameter and return TRUE
            unsafe {
                *process_information = proc_info;
            }
            tracing::debug!("✅ Hook succeeded via unified creation");
            TRUE
        }
        Err(e) => {
            // Failure: log error and fall back to original implementation
            tracing::error!("❌ Unified process creation failed: {}", e);
            // Fallback to original Windows API implementation
            tracing::warn!("🔄 Falling back to original CreateProcessInternalW");

            let result = unsafe {
                original(
                    user_token,
                    application_name,
                    command_line,
                    process_attributes,
                    thread_attributes,
                    inherit_handles,
                    creation_flags,
                    environment,
                    current_directory,
                    startup_info,
                    process_information,
                    restricted_user_token,
                )
            };

            if result != 0 {
                tracing::debug!("✅ Fallback to original API succeeded");
            } else {
                tracing::error!("❌ Both unified creation and fallback failed");
            }

            result
        }
    }
}

/// Initialize process creation hooks.
///
/// Installs the CreateProcessInternalW hook using the detours library to intercept
/// all process creation calls and redirect them through our unified mirrord system.
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    // NOTE(gabriela): handling this at syscall level is super cumbersome
    // and undocumented, so I'd have to reverse engineer CreateProcessInternalW
    // which is like, 3000-ish lines without type fixups, and we shipped this before
    // and in like, 5 years, we had no issues (previous job).
    // so it should work here too.
    apply_hook!(
        guard,
        "kernelbase",
        "CreateProcessInternalW",
        create_process_internal_w_hook,
        CreateProcessInternalWType,
        CREATE_PROCESS_INTERNAL_W_ORIGINAL
    )?;

    Ok(())
}
