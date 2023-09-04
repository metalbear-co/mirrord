use std::ffi::CString;

/// Test the `opendir` hook.
fn main() {
    println!("test issue 1899: START");

    unsafe {
        let dir_path = CString::new("/fake_dir").expect("Valid C string.");
        let dir = libc::opendir(dir_path.into_raw() as *const _);
        assert!(!dir.is_null());
    }

    println!("test issue 1899: SUCCESS");
}
