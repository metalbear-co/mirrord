//! Windows process creation hooks module.
//!
//! This module provides hooks for intercepting Windows process creation
//! and injecting the mirrord layer DLL into child processes for transparent
//! system call interception.
pub mod ops;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::process::windows::execution::CreateProcessInternalWType;
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID, TRUE},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
    },
    um::{
        minwinbase::LPSECURITY_ATTRIBUTES,
        processthreadsapi::{LPPROCESS_INFORMATION, LPSTARTUPINFOW},
        winnt::PHANDLE,
    },
};

use crate::apply_hook;
use ops::{create_process_with_layer_injection, CREATE_PROCESS_INTERNAL_W_ORIGINAL};

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

    // Attempt unified mirrord process creation with layer injection
    match create_process_with_layer_injection(
        application_name,
        command_line,
        current_directory,
        startup_info,
        creation_flags,
        inherit_handles,
    ) {
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

            let original = match CREATE_PROCESS_INTERNAL_W_ORIGINAL.get() {
                Some(original_fn) => original_fn,
                None => {
                    tracing::error!(
                        "❌ Original CreateProcessInternalW function not available - hook not properly installed"
                    );
                    return winapi::shared::minwindef::FALSE;
                }
            };

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
