//! Process utilities

use std::ffi::c_void;

use str_win::string_to_u8_buffer;
// Relocated into `utils-win`; re-exported so existing call sites are untouched.
pub use utils_win::modules::{get_module_base, get_module_from_address, get_module_name};
use winapi::um::libloaderapi::GetProcAddress;

pub mod elevation;
pub mod environment;
pub mod macros;
pub mod memory;

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
