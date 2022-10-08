#![feature(vec_into_raw_parts)]

extern crate alloc;
use alloc::ffi::CString;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::prelude::*,
    path::PathBuf,
};

static FILE_CONTENTS: &'static str = "Hello, I am the file you're reading!";
static FILE_PATH: &'static str = "/app/test_file.txt";

fn create_test_file() {
    unsafe {
        let p = CString::new(FILE_PATH).unwrap();
        let fd = libc::open(p.as_ptr(), libc::O_CREAT | libc::O_RDWR);
        println!("fd {fd:#?}");
    };

    let path = PathBuf::new().join("/").join("app").join("test_file.txt");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
        .expect("Open or create test file!");

    let amount = file
        .write(FILE_CONTENTS.as_bytes())
        .expect("Wrote contents while creating test file!");

    assert_eq!(amount, FILE_CONTENTS.len());
}

fn test_open_read_only() {
    OpenOptions::new()
        .read(true)
        .open(FILE_PATH)
        .expect("Open read-only!");
}

fn test_open_read_write() {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(FILE_PATH)
        .expect("Open read/write!");
}

fn test_open_read_contents() {
    let mut file = OpenOptions::new()
        .read(true)
        .open(FILE_PATH)
        .expect("Open read-only!");

    let mut buffer = String::with_capacity(256);
    let amount = file
        .read_to_string(&mut buffer)
        .expect("Read file contents!");

    assert_eq!(amount, FILE_CONTENTS.len());
}

fn test_fgets() {
    let file = OpenOptions::new()
        .read(true)
        .open(FILE_PATH)
        .expect("fgets!");

    let fd = file.as_raw_fd();

    unsafe {
        let mode = CString::new("r").expect("valid C string");
        let (buffer, _length, _capacity) = vec![0i8; 1500].into_raw_parts();
        let file_stream = libc::fdopen(fd, mode.as_ptr());

        if libc::fgets(buffer, 12, file_stream).is_null() {
            let error_code = libc::ferror(file_stream);
            assert_eq!(error_code, 0);
        }
    };
}

fn main() {
    create_test_file();

    test_open_read_only();
    test_open_read_write();
    test_open_read_contents();
    test_fgets();
}
