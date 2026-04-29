//! Process utilities

use std::{ffi::c_void, path::PathBuf};

use str_win::{string_to_u8_buffer, string_to_u16_buffer, u16_buffer_to_string};
use winapi::um::libloaderapi::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleFileNameW, GetModuleHandleExW,
    GetModuleHandleW, GetProcAddress,
};

pub mod elevation;
pub mod environment;
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
pub fn get_export<T: AsRef<str>, U: AsRef<str>>(module: T, export: U) -> *mut c_void {
    let module = get_module_base(module);
    let export = string_to_u8_buffer(export);

    unsafe { GetProcAddress(module as _, export.as_ptr() as _) as *mut c_void }
}

/// Obtains the base address of a module, from an address that is inside that module. `NULL` is
/// returned if not found.
///
/// # Arguments
///
/// * `ptr` - Address that may or may not be inside a module in the process module list.
pub fn get_module_from_address(ptr: *mut c_void) -> *mut c_void {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let mut result: *mut c_void = std::ptr::null_mut();

    let ret = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            ptr as _,
            std::ptr::from_mut(&mut result) as _,
        )
    };
    if ret == 0 {
        std::ptr::null_mut()
    } else {
        result as _
    }
}

/// Obtains the name of a module from it's base address. `None` if it doesn't have one.
///
/// # Arguments
///
/// * `module` - Address that may or may not point to a module.
pub fn get_module_name(module: *mut c_void) -> Option<String> {
    if module.is_null() {
        return None;
    }

    let mut name = [0u16; 512];

    let ret = unsafe { GetModuleFileNameW(module as _, name.as_mut_ptr() as _, name.len() as _) };

    if ret == 0 {
        None
    } else {
        let name = u16_buffer_to_string(name);

        // The result of the operation is actually supposed to be a fully
        // qualified path.
        let path = PathBuf::from(name);

        // Check if it's *actually* a fully quallified path.
        if !path.has_root() {
            return None;
        }

        // Try to extract file name.
        let module_name = path.file_name()?;

        Some(module_name.to_string_lossy().to_string())
    }
}
