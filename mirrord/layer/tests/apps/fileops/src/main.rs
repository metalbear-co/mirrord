#![feature(vec_into_raw_parts)]

extern crate alloc;
use alloc::ffi::CString;
use std::{fs::OpenOptions, os::unix::prelude::*};

static FILE_PATH: &str = "/tmp/test_file.txt";

fn test_pwrite() {
    println!(">> test_pwrite");

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(FILE_PATH)
        .expect("pwrite!");

    let fd = file.as_raw_fd();

    unsafe {
        let data = CString::new("Hello, I am the file you're writing!").expect("valid C string");

        let (buffer, length, _capacity) = data.into_bytes_with_nul().into_raw_parts();

        assert_eq!(libc::pwrite(fd, buffer.cast(), length, 0), 37);
    };
}

// Test that fclose flushes correctly, no need to run remotely for all we care
fn test_ffunctions() {
    println!(">> test_ffunctions");

    unsafe {
        let filepath = CString::new("/tmp/test_file2.txt").unwrap();
        let file_mode = CString::new("w").unwrap();
        let file_ptr = libc::fopen(filepath.as_ptr(), file_mode.as_ptr());
        let data = CString::new("Hello, I am the file you're writing!").expect("valid C string");
        let (buffer, length, _capacity) = data.into_bytes_with_nul().into_raw_parts();
        assert_eq!(libc::fwrite(buffer.cast(), 1, length, file_ptr), length);
        libc::fclose(file_ptr);
    };
}

fn main() {
    test_pwrite();
    test_ffunctions();
}
