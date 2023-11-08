use std::{net::TcpListener, io::Read};

const LISTEN_ON: &'static str = "0.0.0.0:9999";
const EXPECTED_MESSAGE: &'static [u8] = b"HELLO";

fn main() {
    let listener = TcpListener::bind(LISTEN_ON).unwrap();
    let (mut stream, _peer) = listener.accept().unwrap();

    let mut buff = [0_u8; 10];
    let bytes_read = stream.read(&mut buff[..]).unwrap();
    assert_eq!(&buff[..bytes_read], EXPECTED_MESSAGE);
}
