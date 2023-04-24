#![feature(vec_into_raw_parts)]
#![warn(clippy::indexing_slicing)]

extern crate alloc;
use alloc::ffi::CString;
use std::{fs::OpenOptions, os::unix::prelude::*};

static FILE_PATH: &str = "/tmp/test_file.txt";

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

// Test that fclose flushes correctly, no need to run remotely for all we care
fn ffunctions() {
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

// Rust compiles with newer libc on Linux that uses statx
#[cfg(target_os = "macos")]
// Test that lstat works remotely
fn lstat() {
    println!(">> test_lstat");

    let metadata = std::fs::symlink_metadata("/tmp/test_file.txt").unwrap();

    assert_eq!(metadata.dev(), 0);
    assert_eq!(metadata.size(), 1);
    assert_eq!(metadata.uid(), 2);
    assert_eq!(metadata.blocks(), 3);
}

#[cfg(target_os = "macos")]
// Test that stat works remotely
fn stat() {
    println!(">> test_stat");

    let metadata = std::fs::metadata("/tmp/test_file.txt").unwrap();

    assert_eq!(metadata.dev(), 4);
    assert_eq!(metadata.size(), 5);
    assert_eq!(metadata.uid(), 6);
    assert_eq!(metadata.blocks(), 7);
}

fn main() {
    pwrite();
    ffunctions();
    #[cfg(target_os = "macos")]
    {
        lstat();
        stat();
    }
    // let close message get called
    std::thread::sleep(std::time::Duration::from_millis(10));
}
