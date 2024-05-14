//! Binds a TCP socket to `0.0.0.0:0` and then connects to an in-cluster peer at `1.1.1.1:4567`.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::fd::FromRawFd,
};

use nix::sys::socket::{self, AddressFamily, SockFlag, SockType, SockaddrStorage};
fn main() {
    let peer_address = "1.1.1.1:4567".parse::<SocketAddr>().unwrap();
    let bind_address = "0.0.0.0:0".parse::<SocketAddr>().unwrap();

    let sockfd = socket::socket(
        AddressFamily::Inet,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .unwrap();
    println!("SOCKET CREATED: {sockfd}");

    socket::bind(sockfd, &SockaddrStorage::from(bind_address)).unwrap();
    println!("SOCKET BOUND");

    socket::connect(sockfd, &SockaddrStorage::from(peer_address)).unwrap();
    println!("SOCKET CONNECTED");

    let mut stream = unsafe { TcpStream::from_raw_fd(sockfd) };
    assert_eq!(stream.peer_addr().unwrap(), peer_address);
    println!("PEER ADDRESS AS EXPECTED");

    let message = b"hello there";
    let bytes_written = stream.write(message).unwrap();
    assert_eq!(bytes_written, message.len(), "partial write");

    let mut buf = vec![];
    stream.read_to_end(&mut buf).unwrap();
    assert_eq!(buf.as_slice(), message);
    println!("RECEIVED ECHO");
}
