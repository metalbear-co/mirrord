use std::{
    fs::{File, OpenOptions},
    io::{prelude::*, Result, SeekFrom},
    net::{TcpListener, TcpStream},
};

fn main() -> Result<()> {
    let mut file = File::open("/var/log/dpkg.log")?;
    println!("\t>>> open file {file:#?} \n");

    let mut buffer = vec![0; u16::MAX as usize];
    println!("\t>>> preparing to read \n");

    let count = file.read(&mut buffer)?;
    println!("\t>>> read {count:#?} bytes from file \n");

    let new_start = file.seek(SeekFrom::Start(10))?;
    println!("\t>>> seek now starts at {new_start:#?} \n");

    let mut write_file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open("/tmp/meow.txt")
        .unwrap();

    println!("\t>>> created write file {write_file:#?} \n");

    let written_count = write_file.write("Meow meow meow".as_bytes()).unwrap();

    println!("\t>>> written {written_count:#?} bytes to file \n");

    let mut read_buf = vec![0; 16];
    let read_count = write_file.read(&mut read_buf).unwrap();
    println!("\t>>> read {read_count:#?} bytes from file \n");

    let read_str = String::from_utf8_lossy(&read_buf);
    println!("\t>>> file contains message {read_str:#?} \n");

    let listener = TcpListener::bind("127.0.0.1:80")?;
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
