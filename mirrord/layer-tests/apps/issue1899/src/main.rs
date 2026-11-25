#[cfg(target_family = "unix")]
use std::ffi::CString;

/// Test the `opendir` hook.
#[cfg(target_family = "unix")]
fn main() {
    println!("test issue 1899: START");

    unsafe {
        let dir_path = CString::new("/tmp").expect("Valid C string.");
        let dir = libc::opendir(dir_path.into_raw() as *const _);
        assert!(!dir.is_null());
    }

    println!("test issue 1899: SUCCESS");
}

#[cfg(not(target_family = "unix"))]
fn main() {
    eprintln!("ERROR: test issue 1899 is not supported on non-Unix platforms");
    std::process::exit(1);
}
