//! Process utilities

use std::ffi::c_void;

use str_win::{string_to_u8_buffer, string_to_u16_buffer};
use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress};

pub fn get_module_base<T: AsRef<str>>(module: T) -> *mut c_void {
    let module = string_to_u16_buffer(module);

    let base_address = unsafe { GetModuleHandleW(module.as_ptr()) };
    base_address as _
}

pub fn get_export<T: AsRef<str>, U: AsRef<str>>(module: T, export: U) -> *mut c_void {
    let module = get_module_base(module);
    let export = string_to_u8_buffer(export);

    unsafe { GetProcAddress(module as _, export.as_ptr() as _) as *mut c_void }
}
