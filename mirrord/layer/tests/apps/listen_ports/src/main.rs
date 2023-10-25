use std::{
    io::Read,
    net::{SocketAddr, TcpListener},
};

fn main() {
    let mut buf = [0_u8; 5];

    let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
    // Check `listen_ports` work
    let listener = TcpListener::bind(addr).unwrap();
    // verify user could connect on port 51222
    let (mut conn, _) = listener.accept().unwrap();
    conn.read_exact(buf.as_mut_slice()).unwrap();
    assert_eq!(buf.as_slice(), b"HELLO");

    // Check specific port but not in listen_ports works
    let addr: SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let listener = TcpListener::bind(addr).unwrap();
    // verify user could connect on port 40000
    let (mut conn, _) = listener.accept().unwrap();
    conn.read_exact(buf.as_mut_slice()).unwrap();
    assert_eq!(buf.as_slice(), b"HELLO");

    // Checking that random port works is hard because we need to make libc return an error
    // so we miss that for now.
}
