#![cfg(not(target_os = "windows"))]

use alloc::ffi::CString;
use std::{fs::OpenOptions, os::unix::prelude::*};

use crate::FILE_PATH;

pub fn fgets() {
    println!(">> test_fgets");

    let file = OpenOptions::new()
        .read(true)
        .open(FILE_PATH)
        .expect("fgets!");

    let fd = file.as_raw_fd();

    unsafe {
        let mode = CString::new("r").expect("valid C string");
        #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
        let (buffer, _length, _capacity) = vec![0i8; 1500].into_raw_parts();
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let (buffer, _length, _capacity) = vec![0u8; 1500].into_raw_parts();
        let file_stream = libc::fdopen(fd, mode.as_ptr());

        if libc::fgets(buffer, 12, file_stream).is_null() {
            let error_code = libc::ferror(file_stream);
            assert_eq!(error_code, 0);
        }
    };
}

pub fn pread() {
    println!(">> test_pread");

    let file = OpenOptions::new()
        .read(true)
        .open(FILE_PATH)
        .expect("pread!");

    let fd = file.as_raw_fd();

    unsafe {
        let mode = CString::new("r").expect("valid C string");
        let (buffer, length, _capacity) = vec![0i8; 1500].into_raw_parts();

        if libc::pread(fd, buffer.cast(), length, 1) < 1 {
            let file_stream = libc::fdopen(fd, mode.as_ptr());
            let error_code = libc::ferror(file_stream);
            assert_eq!(error_code, 0);
        }
    };
}

pub fn pwrite() {
    println!(">> test_pwrite");

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(FILE_PATH)
        .expect("pwrite!");

    let fd = file.as_raw_fd();

    unsafe {
        let data = CString::new("Hello, I am the file you're writing!").expect("valid C string");

        let (buffer, length, _capacity) = data.into_bytes_with_nul().into_raw_parts();

        assert_eq!(libc::pwrite(fd, buffer.cast(), length, 0), 37);
    };
}
