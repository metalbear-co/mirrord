use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{prelude::*, Result, SeekFrom},
    mem::size_of,
    net::{TcpListener, TcpStream},
};

fn debug_request() {
    let body = reqwest::blocking::get("https://www.rust-lang.org")
        .unwrap()
        .text()
        .unwrap();

    println!("body = {:#?}", body);
}

fn main() -> Result<()> {
    debug_request();

    Ok(())
}

fn handle_connection(mut stream: TcpStream) {
    let mut buffer = [0; 1024];

    let _ = stream.read(&mut buffer).unwrap();

    println!(">>>>> request is {}", String::from_utf8_lossy(&buffer[..]));
}
