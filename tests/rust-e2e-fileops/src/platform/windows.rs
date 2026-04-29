use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
};

use crate::FILE_PATH;

pub fn fgets() {
    println!(">> test_fgets");

    let mut file = File::open(FILE_PATH).expect("Failed to open file");
    let mut buffer = [0u8; 12];

    file.read_exact(&mut buffer)
        .expect("Failed to read from file");
}

pub fn pread() {
    println!(">> test_pread");

    let mut file = File::open(FILE_PATH).expect("Failed to open file");
    let mut buffer = vec![0u8; 1500];

    // Seek to offset 1
    file.seek(SeekFrom::Start(1)).expect("Failed to seek");

    file.read_exact(&mut buffer)
        .expect("Failed to read from file");
}

pub fn pwrite() {
    println!(">> test_pwrite");

    let mut file = OpenOptions::new()
        .write(true)
        .open(FILE_PATH)
        .expect("Failed to open file for writing");

    let data = "Hello, I am the file you're writing!";

    // Seek to beginning
    file.seek(SeekFrom::Start(0))
        .expect("Failed to seek to start");

    let bytes_written = file.write(data.as_bytes()).expect("Failed to write");
    assert_eq!(bytes_written, data.len());
}
