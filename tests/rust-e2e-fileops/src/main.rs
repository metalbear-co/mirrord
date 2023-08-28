#![feature(vec_into_raw_parts)]
#![warn(clippy::indexing_slicing)]

extern crate alloc;
use alloc::ffi::CString;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::prelude::*,
};

static FILE_CONTENTS: &str = "Hello, I am the file you're reading!";
static FILE_PATH: &str = "/tmp/test_file.txt";

fn create_test_file() {
    println!(">> Creating test file.");

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

fn open_read_only() {
    println!(">> test_open_read_only");

    OpenOptions::new()
        .read(true)
        .open(FILE_PATH)
        .expect("Open read-only!");
}

fn open_read_write() {
    println!(">> test_open_read_write");

    OpenOptions::new()
        .read(true)
        .write(true)
        .open(FILE_PATH)
        .expect("Open read/write!");
}

fn open_read_contents() {
    println!(">> test_open_read_contents");

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

fn fgets() {
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

fn pread() {
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

fn pwrite() {
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

fn main() {
    create_test_file();

    open_read_only();
    open_read_write();
    open_read_contents();
    fgets();
    pread();
    pwrite();
}
