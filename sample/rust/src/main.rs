#![allow(dead_code)]
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{prelude::*, SeekFrom},
    mem::size_of,
};

fn main() {
    // debug_file_ops().expect("Failed debugging file_ops!");
    debug_outgoing_request().expect("Failed debugging outgoing_request!");
}

fn debug_outgoing_request() -> Result<(), reqwest::Error> {
    let client = reqwest::blocking::ClientBuilder::new().build()?;
    println!(">>>>> client built");

    let body = client.get("https://www.rust-lang.org").send()?.text()?;
    println!(">>>>> response body {:#?}", &body[0..10]);

    Ok(())
}

fn debug_file_ops() -> Result<(), std::io::Error> {
    let mut file = File::open("/var/log/dpkg.log")?;
    println!(">>>>> open file {:#?}", file);

    let mut buffer = vec![0; u16::MAX as usize];
    let read_count = file.read(&mut buffer)?;
    println!(">>>>> read {:#?} bytes from file {:#?}", read_count, file);

    let new_start = file.seek(SeekFrom::Start(10))?;
    println!(
        ">>> seek now starts at {:#?} for file {:#?}",
        new_start, file
    );

    let mut write_file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open("/tmp/rust_sample.txt")
        .unwrap();
    println!(">>>>> created file with write permission {:#?}", write_file);

    let written_count = write_file.write("mirrord sample rust".as_bytes()).unwrap();
    println!(
        ">>>>> written {:#?} bytes to file {:#?}",
        written_count, write_file
    );

    write_file.seek(SeekFrom::Start(0)).unwrap();
    println!(">>>>> seeking file back to start {:#?}", write_file);

    let mut read_buf = vec![0; 128];
    let read_count = write_file.read(&mut read_buf).unwrap();
    println!(
        ">>>>> read {:#?} bytes from file {:#?}",
        read_count, write_file
    );

    let read_str = String::from_utf8_lossy(&read_buf);
    println!(">>>>> read {:#?} from file {:#?}", read_str, write_file);

    unsafe {
        let filepath = CString::new("/tmp/rust_sample.txt").unwrap();
        let file_mode = CString::new("r").unwrap();
        let file_ptr = libc::fopen(filepath.as_ptr(), file_mode.as_ptr());

        let file_fd: i32 = *(file_ptr as *const _);
        println!(">>>>> fopen local fd {:#?}", file_fd);

        let mut buffer = vec![0; 128];
        let read_amount = libc::fread(buffer.as_mut_ptr().cast(), size_of::<u8>(), 128, file_ptr);
        println!(
            ">>>>> fread read {:#?} bytes from file {:#?}",
            read_amount, filepath
        );

        let read_str = String::from_utf8_lossy(&buffer);
        println!(">>>>> read {:#?} from file {:#?}", read_str, filepath);
    }

    let dir = File::open("/var/log/")?;
    println!(
        ">>>>> open directory {:#?} with {:#?} and is directory ? {:#?} ",
        dir,
        dir.metadata(),
        dir.metadata().unwrap().is_dir()
    );

    Ok(())
}
