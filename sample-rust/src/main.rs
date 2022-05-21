use std::{
    fs::File,
    io::{prelude::*, Result, SeekFrom},
    net::{TcpListener, TcpStream},
};

fn main() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:80")?;

    let mut file = File::open("/var/log/dpkg.log")?;
    println!("\t>>> open file {file:#?} \n");

    let mut buffer = vec![0; u16::MAX as usize];
    let count = file.read(&mut buffer)?;
    println!("\t>>> read {count:#?} bytes from file \n");

    let new_start = file.seek(SeekFrom::Start(10))?;
    println!("\t>>> seek now starts at {new_start:#?} \n");

    for stream in listener.incoming() {
        let stream = stream.unwrap();

        handle_connection(stream);
    }

    Ok(())
}

fn handle_connection(mut stream: TcpStream) {
    let mut buffer = [0; 1024];

    stream.read(&mut buffer).unwrap();

    println!("Request: {}", String::from_utf8_lossy(&buffer[..]));
}
