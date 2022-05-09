use std::{
    io::Read,
    net::{TcpListener, TcpStream},
};

fn main() {
    let listener = TcpListener::bind("localhost:7777").unwrap();
    listener.take_error().expect("No error here!");
    println!("Listening... {listener:#?}");

    for stream in listener.incoming() {
        let stream = stream.unwrap();
        handle_connection(stream);

        println!("Connection established!");
    }

    println!("finished program");
}

fn handle_connection(mut stream: TcpStream) {
    println!("handle_connection");

    let mut buffer = [0; 1024];

    stream.read(&mut buffer).unwrap();

    println!("Request: {}", String::from_utf8_lossy(&buffer[..]));
}
