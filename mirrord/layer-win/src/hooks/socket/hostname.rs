//! Hostname-related utilities for Windows socket hooks
//!
//! This module contains all the hostname manipulation, DNS resolution, and caching
//! logic used by the Windows socket hooks, separated from the actual hook implementations.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::result_large_err)]

use mirrord_layer_lib::{
    error::HookResult,
    socket::hostname::{get_remote_netbios_name, remote_hostname_string},
};
use winapi::{
    shared::winerror::{ERROR_INVALID_PARAMETER, WSAEFAULT},
    um::{errhandlingapi::SetLastError, winsock2::WSASetLastError},
};

use super::utils::validate_buffer_params;

/// Reasonable buffer limit for hostname functions to prevent abuse
pub const REASONABLE_BUFFER_LIMIT: usize = 32 * 8; // 256 bytes

/// Get hostname for specific Windows computer name type
///
/// This function handles the logic for determining what hostname to return
/// based on the Windows GetComputerNameEx name_type parameter.
///
/// Returns None if the hostname feature is disabled or if no hostname is available.
pub fn get_hostname_for_name_type(name_type: u32) -> HookResult<Option<String>> {
    use winapi::um::sysinfoapi::{
        ComputerNameDnsDomain, ComputerNameMax, ComputerNameNetBIOS, ComputerNamePhysicalDnsDomain,
        ComputerNamePhysicalNetBIOS,
    };
    // Handle invalid name_type (ComputerNameMax and beyond)
    if name_type >= ComputerNameMax {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
        }
        return Ok(None);
    }

    // take care of the edge cases
    match name_type {
        ComputerNameDnsDomain | ComputerNamePhysicalDnsDomain => {
            // Domain variants - return empty string (no domain info available)
            tracing::debug!("Returning empty domain name for name_type {}", name_type);
            return Ok(Some(String::new()));
        }
        ComputerNameNetBIOS | ComputerNamePhysicalNetBIOS => {
            // NetBIOS variants - attempt to retrieve Samba netbios compatible name
            if let Ok(Some(netbios_name)) = get_remote_netbios_name()
                && !netbios_name.is_empty()
            {
                let result = netbios_name.to_uppercase();
                tracing::debug!(
                    "Got NetBIOS name from Samba config for name_type {}: '{}'",
                    name_type,
                    result
                );
                return Ok(Some(result));
            }
            // if failed, proceed to hostname retrieval
        }
        // for the rest of the cases, proceed to hostname retrieval
        _ => (),
    }

    // rest of the name_types and if netbios failed -
    //  regular hostname resolution from /etc/hostname
    remote_hostname_string(true)
}

/// Generic hostname function for ANSI versions
pub fn handle_hostname_ansi<F, H>(
    lpBuffer: *mut i8,
    nSize: *mut u32,
    original_fn: F,
    get_hostname_fn: H,
    function_name: &str,
    err_buffer_overflow: u32,
    ret_vals: (i32, i32),
) -> i32
where
    F: FnOnce() -> i32,
    H: FnOnce() -> HookResult<Option<String>>,
{
    tracing::debug!("{} hook called", function_name);
    let (ret_ok, ret_err) = ret_vals;

    if let Some(buffer_size) =
        validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
    {
        if let Ok(Some(hostname)) = get_hostname_fn() {
            // Try to get remote hostname
            let hostname_bytes = hostname.as_bytes();
            let hostname_with_null: Vec<u8> = hostname_bytes.to_vec();

            if hostname_with_null.len() > buffer_size {
                tracing::debug!(
                    "{}: buffer too small need {} chars, have {}",
                    function_name,
                    hostname_with_null.len(),
                    buffer_size
                );
                unsafe {
                    *nSize = (hostname_with_null.len() + 1) as u32;
                    SetLastError(err_buffer_overflow);
                    WSASetLastError(WSAEFAULT.try_into().unwrap());
                }
                // FALSE - Buffer too small
                return ret_err;
            }

            unsafe {
                std::ptr::copy_nonoverlapping(
                    hostname_with_null.as_ptr() as *const i8,
                    lpBuffer,
                    hostname_with_null.len(),
                );

                // Add null terminator - safe because we verified buffer size above
                *(lpBuffer.add(hostname_with_null.len())) = 0;
                // Set actual length
                *nSize = hostname_with_null.len() as u32;
            }
            tracing::debug!("{} returning DNS hostname: {}", function_name, hostname);
            // TRUE - Success
            return ret_ok;
        }
        // hostname resolution failed, fallback to original
    } else {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
            WSASetLastError(WSAEFAULT.try_into().unwrap());
        }
        // FALSE
        return ret_err;
    }

    // Fall back to original function (hostname feature disabled or no remote hostname)
    tracing::debug!(
        "{}: using original function (hostname feature disabled or no remote hostname)",
        function_name
    );
    original_fn()
}

/// Generic hostname function for Unicode versions
/// Note: does not support WinSock error reporting, see ansi version
pub fn handle_hostname_unicode<F, H>(
    lpBuffer: *mut u16,
    nSize: *mut u32,
    original_fn: F,
    get_hostname_fn: H,
    function_name: &str,
    err_buffer_overflow: u32,
) -> i32
where
    F: FnOnce() -> i32,
    H: FnOnce() -> HookResult<Option<String>>,
{
    tracing::debug!("{} hook called", function_name);

    if let Some(buffer_size) =
        validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
    {
        if let Ok(Some(hostname)) = get_hostname_fn() {
            let name_utf16: Vec<u16> = hostname.encode_utf16().collect();

            if name_utf16.len() > buffer_size {
                // Buffer too small - set required size and return `err_buffer_overflow`
                unsafe {
                    // Include null terminator
                    *nSize = (name_utf16.len() + 1) as u32;

                    // set error appropriate for the current hooked api
                    SetLastError(err_buffer_overflow);
                }
                // FALSE - Buffer too small
                return 0;
            }

            unsafe {
                std::ptr::copy_nonoverlapping(name_utf16.as_ptr(), lpBuffer, name_utf16.len());
                // Add UTF-16 null terminator - safe because we verified buffer size above
                *(lpBuffer.add(name_utf16.len())) = 0;
                // Set actual length
                *nSize = name_utf16.len() as u32;
            }
            tracing::debug!("{} returning hostname: {}", function_name, hostname);
            // TRUE - Success
            return 1;
        }
        // failed to get hostname? fallback to original
    } else {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
        }
        // FALSE
        return 0;
    }

    tracing::debug!(
        "{}: Using original function (hostname feature disabled or remote resolution failed)",
        function_name
    );
    original_fn()
}

/// Helper function to check if a hostname matches our remote hostname
pub fn is_remote_hostname(hostname: String) -> bool {
    // skip hostname enabled test for is_remote_hostname
    remote_hostname_string(false)
        .is_ok_and(|opt| opt.is_some_and(|remote_hostname| remote_hostname == hostname))
        || get_remote_netbios_name()
            .is_ok_and(|opt| opt.is_some_and(|remote_netbios| remote_netbios == hostname))
}
