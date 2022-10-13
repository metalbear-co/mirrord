#![feature(vec_into_raw_parts)]

extern crate alloc;
use alloc::ffi::CString;
use std::{fs::OpenOptions, io::Write, os::unix::prelude::*};

static FILE_CONTENTS: &str = "Hello, I am the file you're reading!";
static FILE_PATH: &str = "/tmp/test_file.txt";

fn create_test_file() {
    println!(">> Creating test file");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(FILE_PATH)
        .expect("Open or create test file!");

    let amount = file
        .write(FILE_CONTENTS.as_bytes())
        .expect("Wrote contents while creating test file!");

    assert_eq!(amount, FILE_CONTENTS.len());
}
fn test_pwrite() {
    println!(">> test_pwrite");

    let file = OpenOptions::new()
        .write(true)
        .open(FILE_PATH)
        .expect("pwrite!");

    let fd = file.as_raw_fd();

    unsafe {
        let mode = CString::new("rw").expect("valid C string");

        let data = CString::new("Hello, I am the file you're writing!").expect("valid C string");

        let (buffer, length, _capacity) = data.into_bytes_with_nul().into_raw_parts();

        if libc::pwrite(fd, buffer.cast(), length, 12) < 1 {
            let file_stream = libc::fdopen(fd, mode.as_ptr());
            let error_code = libc::ferror(file_stream);
            assert_eq!(error_code, 0);
        }
    };
}

fn main() {
    create_test_file();
    test_pwrite();
}
