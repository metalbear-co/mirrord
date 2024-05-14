//! Attempts to connect to in-cluster peer `1.1.1.1:4567` from bound TCP sockets. Tests 2 cases:
//! 1. Socket is first bound to an address `0.0.0.0:0`, that triggers a bypass in `bind` detour
//!    (socket **could** later be used for port subscription)
//! 2. Socket is first bound to an address `0.0.0.0:80`, that goes through `bind` detour completely
//!    (socket **could not** later be used for port subscription)

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::fd::FromRawFd,
};

use nix::sys::socket::{self, AddressFamily, SockFlag, SockType, SockaddrStorage};
fn main() {
    let bind_addresses: [SocketAddr; 2] =
        ["0.0.0.0:0".parse().unwrap(), "0.0.0.0:80".parse().unwrap()];

    let peer_address = "1.1.1.1:4567".parse::<SocketAddr>().unwrap();

    for bind_address in bind_addresses {
        let sockfd = socket::socket(
            AddressFamily::Inet,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .unwrap();
        println!("SOCKET CREATED: {sockfd}");

        socket::bind(sockfd, &SockaddrStorage::from(bind_address)).unwrap();
        println!("SOCKET BOUND TO {bind_address}");

        socket::connect(sockfd, &SockaddrStorage::from(peer_address)).unwrap();
        println!("SOCKET CONNECTED TO {peer_address}");

        let mut stream = unsafe { TcpStream::from_raw_fd(sockfd) };
        assert_eq!(stream.peer_addr().unwrap(), peer_address);
        println!("`TcpStream::peer_addr()` RESULT AS EXPECTED");

        let message = b"hello there";
        let bytes_written = stream.write(message).unwrap();
        assert_eq!(bytes_written, message.len(), "partial write");

        let mut buf = vec![];
        stream.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), message);
        println!("RECEIVED ECHO");
    }
}
