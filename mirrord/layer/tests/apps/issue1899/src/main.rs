#[cfg(unix)]
use std::ffi::CString;

/// Test the `opendir` hook.
#[cfg(unix)]
fn main() {
    println!("test issue 1899: START");

    unsafe {
        let dir_path = CString::new("/tmp").expect("Valid C string.");
        let dir = libc::opendir(dir_path.into_raw() as *const _);
        assert!(!dir.is_null());
    }

    println!("test issue 1899: SUCCESS");
}

#[cfg(not(unix))]
fn main() {
    // This test is Unix-specific and does nothing on other platforms
    println!("issue1899 test skipped on non-Unix platforms");
}
