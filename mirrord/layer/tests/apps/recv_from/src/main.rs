use std::{mem::MaybeUninit, net::SocketAddr};

use socket2::{Domain, Socket, Type};

fn main() {
    let address: SocketAddr = "1.2.3.4:4367".parse().unwrap();

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None).expect("Failed to create socket");

    socket
        .connect(&address.into())
        .expect("Failed to connect to socket");

    let data = "Hello, world!";

    socket.send(data.as_bytes()).expect("Failed to send query");

    let mut response = [MaybeUninit::<u8>::uninit(); 1024];
    let (len, source_address) = socket
        .recv_from(&mut response)
        .expect("Failed to receive response");

    let response = response[..len]
        .iter()
        .map(|b| unsafe { b.assume_init() })
        .collect::<Vec<u8>>();

    assert_eq!(
        String::from_utf8(response).expect("Failed to parse response"),
        "Hello, world!"
    );

    assert_eq!(
        source_address.as_socket_ipv4().unwrap().to_string(),
        "1.2.3.4:4367"
    );
}
