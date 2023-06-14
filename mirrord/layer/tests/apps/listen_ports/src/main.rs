#![warn(clippy::indexing_slicing)]

use std::{
    net::{SocketAddr, TcpListener},
};



fn main() {
    let addr: SocketAddr = "127.0.0.1.80".parse().unwrap()
    // Check `listen_ports` work
    let listener = TcpListener::bind(addr).unwrap();
    // verify user could connect on port 51222
    listener.accept().unwrap();

    // Check specific port but not in listen_ports works
    let addr: SocketAddr = "127.0.0.1.40000".parse().unwrap();
    let listener = TcpListener::bind(addr).unwrap();
    // verify user could connect on port 40000
    listener.accept().unwrap();

    // Checking that random port works is hard because we need to make libc return an error
    // so we miss that for now.
}
