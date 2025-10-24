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
    ctypes::c_void,
    shared::{
        minwindef::{BOOL, DWORD, HMODULE, LPVOID, TRUE, FALSE},
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

use crate::{apply_hook, process::environment::parse_environment_block};

/// Static storage for the original CreateProcessInternalW function pointer.
static CREATE_PROCESS_INTERNAL_W_ORIGINAL: OnceLock<&CreateProcessInternalWType> = OnceLock::new();

// LoadLibrary hook to detect module loading for super verbose debugging
type LoadLibraryWType = unsafe extern "system" fn(lpLibFileName: *const u16) -> HMODULE;
static LOAD_LIBRARY_W_ORIGINAL: OnceLock<&LoadLibraryWType> = OnceLock::new();

// GetProcAddress hook to detect API usage for super verbose debugging
type GetProcAddressType =
    unsafe extern "system" fn(hModule: HMODULE, lpProcName: *const i8) -> *mut c_void;
static GET_PROC_ADDRESS_ORIGINAL: OnceLock<&GetProcAddressType> = OnceLock::new();

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

    // Get the original function pointer
    let original = match CREATE_PROCESS_INTERNAL_W_ORIGINAL.get() {
        Some(original_fn) => original_fn,
        None => {
            tracing::error!(
                "❌ Original CreateProcessInternalW function not available - hook not properly installed"
            );
            return FALSE;
        }
    };

    // Parse environment from Windows API call - check creation flags for format
    let env_vars =
        unsafe { parse_environment_block(environment as *mut _, creation_flags) };

    tracing::debug!(
        "Windows CreateProcess parameters: parent_pid={}, env_count={}",
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

        if success != 0 {
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

/// Hook LoadLibraryW for super verbose API monitoring during injection
///
/// This detour monitors DLL loading to provide comprehensive visibility into
/// what modules are being loaded during process injection. Useful for debugging
/// complex injection scenarios and understanding the full API landscape.
unsafe extern "system" fn loadlibrary_w_detour(lpLibFileName: *const u16) -> HMODULE {
    // Call the original LoadLibraryW first
    let original = LOAD_LIBRARY_W_ORIGINAL.get().unwrap();
    let result = unsafe { original(lpLibFileName) };

    // Always log LoadLibrary calls for super verbose debugging
    if !lpLibFileName.is_null() {
        // Convert the wide string to a Rust string for logging
        let len = unsafe {
            (0..).take_while(|&i| *lpLibFileName.offset(i) != 0).count()
        };

        if len > 0 {
            let wide_slice = unsafe { std::slice::from_raw_parts(lpLibFileName, len) };
            let lib_name = String::from_utf16_lossy(wide_slice);

            // Super verbose logging - trace every library load
            tracing::trace!(
                "LoadLibraryW: module='{}' handle={:?}",
                lib_name,
                result
            );
        }
    }

    result
}

/// Hook GetProcAddress for super verbose API monitoring during injection
///
/// This detour monitors function pointer acquisition to provide comprehensive
/// visibility into what APIs are being resolved during process injection.
/// Essential for understanding the complete API usage pattern.
unsafe extern "system" fn getprocaddress_detour(
    hModule: HMODULE,
    lpProcName: *const i8,
) -> *mut c_void {
    let original = GET_PROC_ADDRESS_ORIGINAL.get().unwrap();

    // Always call original first to get the function address
    let original_result = unsafe { original(hModule, lpProcName) };

    // Only process if we have a valid function name pointer
    if !lpProcName.is_null() {
        // Convert the function name to a string for logging
        if let Ok(function_name) = unsafe { std::ffi::CStr::from_ptr(lpProcName) }.to_str() {
            // Super verbose logging - trace every function resolution
            tracing::trace!(
                "GetProcAddress: module={:?} function='{}' address={:?}",
                hModule,
                function_name,
                original_result
            );
        }
    }

    original_result
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

    // API monitoring hooks for debugging injection scenarios
    tracing::debug!("Installing API tracing hooks for LoadLibraryW and GetProcAddress");
    
    apply_hook!(
        guard,
        "kernel32",
        "LoadLibraryW",
        loadlibrary_w_detour,
        LoadLibraryWType,
        LOAD_LIBRARY_W_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "kernel32",
        "GetProcAddress",
        getprocaddress_detour,
        GetProcAddressType,
        GET_PROC_ADDRESS_ORIGINAL
    )?;

    Ok(())
}
