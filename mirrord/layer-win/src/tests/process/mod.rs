use std::ffi::c_void;

use str_win::string_to_u16_buffer;

use crate::process::get_export;

#[test]
fn check_kernel32() {
    unsafe {
        type GetModuleHandleWType = unsafe extern "system" fn(*const u16) -> *mut c_void;
        let ffi_get_module_handle: GetModuleHandleWType =
            std::mem::transmute(get_export("kernel32", "GetModuleHandleW"));

        let module_name_buf = string_to_u16_buffer("kernel32");
        let base_address = ffi_get_module_handle(module_name_buf.as_ptr());
        assert!(!base_address.is_null());
    }
}
