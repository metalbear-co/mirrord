//! Windows process creation hooks module.
//!
//! This module provides hooks for intercepting Windows process creation
//! and injecting the mirrord layer DLL into child processes for transparent
//! system call interception.

use std::sync::OnceLock;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult, windows::WindowsError},
    process::windows::execution::{CreateProcessInternalWType, LayerManagedProcess},
};
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{BOOL, DWORD, HMODULE, LPVOID, TRUE},
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

use crate::{
    apply_hook,
    process::{environment::parse_environment_block, get_module_name},
};

static CREATE_PROCESS_INTERNAL_W_ORIGINAL: OnceLock<&CreateProcessInternalWType> = OnceLock::new();

// LoadLibrary hook to detect module loading during injection
type LoadLibraryWType = unsafe extern "system" fn(lpLibFileName: *const u16) -> HMODULE;
static LOAD_LIBRARY_W_ORIGINAL: OnceLock<&LoadLibraryWType> = OnceLock::new();

// GetProcAddress hook to detect API usage during injection
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
    tracing::debug!("CreateProcessInternalW hook intercepted process creation");

    // Get the original function pointer
    let original = CREATE_PROCESS_INTERNAL_W_ORIGINAL.get().unwrap();

    // Parse environment from Windows API call - check creation flags for format
    let env_vars = unsafe { parse_environment_block(environment as *mut _, creation_flags) };

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
        None::<mirrord_progress::NullProgress>, // No progress in hook context
    )
    .map(|managed_process| managed_process.release())
    {
        Ok(proc_info) => {
            // Success: populate output parameter and return TRUE
            unsafe {
                *process_information = proc_info;
            }
            tracing::debug!("Hook succeeded via unified creation");
            TRUE
        }
        Err(e) => {
            // Failure: log error and fall back to original implementation
            tracing::error!("Unified process creation failed: {}", e);
            // Fallback to original Windows API implementation
            tracing::warn!("Falling back to original CreateProcessInternalW");

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
                tracing::debug!("Fallback to original API succeeded");
            } else {
                tracing::error!("Both unified creation and fallback failed");
            }

            result
        }
    }
}

/// Hook LoadLibraryW for API monitoring during injection
///
/// This detour monitors DLL loading to provide comprehensive visibility into
/// what modules are being loaded during process injection. Useful for debugging
/// complex injection scenarios and understanding the full API landscape.
unsafe extern "system" fn loadlibrary_w_detour(lpLibFileName: *const u16) -> HMODULE {
    // Call the original LoadLibraryW first
    let original = LOAD_LIBRARY_W_ORIGINAL.get().unwrap();
    let result = unsafe { original(lpLibFileName) };

    // Log LoadLibrary calls for debugging
    if !lpLibFileName.is_null() {
        // Convert the wide string to a Rust string for logging
        let lib_name = unsafe { str_win::u16_ptr_to_string(lpLibFileName) };

        if !lib_name.is_empty() {
            // Trace library loading for debugging
            tracing::trace!("LoadLibraryW: module='{}' handle={:?}", lib_name, result);
        }
    }

    result
}

/// Hook GetProcAddress for API monitoring during injection
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

    // NOTE(gabriela): lpProcName may be either an ordinal, or pointer to string.
    // NOTE(gabriela): check win-57
    if !lpProcName.is_null() {
        // NOTE(gabriela): convoluted explication below...
        //
        // `#define MAKEINTRESOURCEA(i) ((LPSTR)((ULONG_PTR)((WORD)(i))))``
        // MAKEINTRESOURCEA only keeps the LOWORD of the input.
        //
        // also check out PE spec https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#export-ordinal-table
        // "A 16-bit ordinal number. This field is used only if the Ordinal/Name Flag bit field is 1
        // (import by ordinal). Bits 30-15 or 62-15 must be 0."
        //

        let is_ordinal = !lpProcName.is_null() && (lpProcName as usize) <= u16::MAX as usize;

        let ordinal = is_ordinal.then_some(lpProcName as u16);

        if let Some(number) = ordinal {
            tracing::trace!(
                "GetProcAddress: module={:?} ptr={:?} ordinal='{}' address={:?}",
                get_module_name(hModule as _),
                hModule,
                number,
                original_result
            );
        } else {
            // Convert the function name to a string for logging
            let function_name = unsafe { str_win::u8_ptr_to_string(lpProcName) };

            if !function_name.is_empty() {
                // Trace function resolution for debugging
                tracing::trace!(
                    "GetProcAddress: module={:?} ptr={:?} function='{}' address={:?}",
                    get_module_name(hModule as _),
                    hModule,
                    function_name,
                    original_result
                );
            }
        }
    }

    original_result
}

/// Initialize process creation hooks.
///
/// Installs the CreateProcessInternalW hook using the detours library to intercept
/// all process creation calls and redirect them through our unified mirrord system.
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
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

    tracing::info!("Process hooks initialization completed");

    Ok(())
}
