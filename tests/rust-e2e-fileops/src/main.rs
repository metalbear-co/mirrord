#![feature(vec_into_raw_parts)]

extern crate alloc;
use alloc::ffi::CString;
use std::{fs::File, os::unix::prelude::*};

fn test_fgets() {
    let file = File::open("/app/test.txt").expect("test.txt must be available!");
    let fd = file.as_raw_fd();

    unsafe {
        let mode = CString::new("r").expect("valid C string");
        let (buffer, _length, _capacity) = vec![0i8; 1500].into_raw_parts();
        let file_stream = libc::fdopen(fd, mode.as_ptr());

        if libc::fgets(buffer, 12, file_stream).is_null() {
            panic!("`fgets` returned a NULL string!");
        }
    };
}

fn main() {
    test_fgets();
}
