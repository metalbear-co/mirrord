use std::ffi::{CStr, CString};

/// Test the `readdir` and `readdir64` hooks.
fn main() {
    println!("test issue 2001: START");

    let dir_path = CString::new("/tmp").unwrap();

    unsafe {
        let dir = libc::opendir(dir_path.into_raw() as *const _);
        assert!(!dir.is_null());

        let dirent_1 = libc::readdir(dir);
        assert!(!dirent_1.is_null());
        let name = CStr::from_ptr((*dirent_1).d_name.as_ptr())
            .to_str()
            .unwrap();
        assert_eq!(name, "file1");

        #[cfg(not(target_os = "macos"))]
        let dirent_2 = libc::readdir64(dir);
        #[cfg(target_os = "macos")]
        let dirent_2 = libc::readdir(dir);
        assert!(!dirent_2.is_null());
        let name = CStr::from_ptr((*dirent_2).d_name.as_ptr())
            .to_str()
            .unwrap();
        assert_eq!(name, "file2");
    }

    println!("test issue 2001: SUCCESS");
}
