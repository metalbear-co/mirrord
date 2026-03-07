#![warn(clippy::indexing_slicing)]

extern crate alloc;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
};

mod platform;

static FILE_CONTENTS: &str = "Hello, I am the file you're reading!";

#[cfg(not(target_os = "windows"))]
static FILE_PATH: &str = "/tmp/test_file.txt";

#[cfg(target_os = "windows")]
static FILE_PATH: &str = r"C:\Windows\Temp\test_file.txt";

fn create_test_file() {
    println!(">> Creating test file.");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
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
    platform::fgets();
}

fn pread() {
    platform::pread();
}

fn pwrite() {
    platform::pwrite();
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
