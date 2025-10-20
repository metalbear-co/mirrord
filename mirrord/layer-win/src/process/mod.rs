//! Process utilities

use std::ffi::c_void;

use str_win::{string_to_u8_buffer, string_to_u16_buffer, u16_multi_buffer_to_strings};
use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress};

pub mod elevation;
pub mod macros;
pub mod memory;

/// Obtains the base address of a module. `NULL` is returned if not found.
///
/// # Arguments
///
/// * `module` - Name of the module to retrieve base address for.
pub fn get_module_base<T: AsRef<str>>(module: T) -> *mut c_void {
    let module = string_to_u16_buffer(module);

    let base_address = unsafe { GetModuleHandleW(module.as_ptr()) };
    base_address as _
}

/// Retrieve an export from module. `NULL` is returned if not found.
///
/// # Arguments
///
/// * `module` - Name of the module to look for exports in.
/// * `export` - Name of export to look for (function/variable/...).
///
/// Retrieve an export from module. `NULL` is returned if not found.
///
/// # Arguments
///
/// * `module` - Name of the module to look for exports in.
/// * `export` - Name of export to look for (function/variable/...).
pub fn get_export<T: AsRef<str>, U: AsRef<str>>(module: T, export: U) -> *mut c_void {
    let module = get_module_base(module);
    let export = string_to_u8_buffer(export);

    unsafe { GetProcAddress(module as _, export.as_ptr() as _) as *mut c_void }
}

/// Parse Windows environment block into HashMap
///
/// The environment block is a null-terminated block of null-terminated strings.
/// Each environment variable is in the form name=value.
///
/// # Safety
/// This function is unsafe because it dereferences raw pointers from the Windows API.
/// The caller must ensure that the environment pointer is valid and properly formatted.
pub unsafe fn parse_environment_block(
    environment: *mut c_void,
) -> std::collections::HashMap<String, String> {
    if environment.is_null() {
        return std::collections::HashMap::new();
    }

    // Convert to u16 pointer for UTF-16 processing
    let env_ptr = environment as *const u16;

    // Find the size by counting until double null terminator
    let size = (0..)
        .take_while(|&i| unsafe { !(*env_ptr.offset(i) == 0 && *env_ptr.offset(i + 1) == 0) })
        .count()
        + 2; // Include the double null terminator
    if size == 0 {
        return std::collections::HashMap::new();
    }

    // Create slice and use str-win utility to parse all environment strings
    let env_slice = unsafe { std::slice::from_raw_parts(env_ptr, size) };

    // Use str-win to parse multi-string buffer and convert to HashMap
    u16_multi_buffer_to_strings(env_slice)
        .into_iter()
        .filter_map(|env_string| {
            env_string
                .split_once('=')
                .map(|(name, value)| (name.to_string(), value.to_string()))
        })
        .collect()
}
