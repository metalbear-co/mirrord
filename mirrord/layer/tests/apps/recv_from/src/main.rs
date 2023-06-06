use std::{mem::MaybeUninit, net::ToSocketAddrs};

use socket2::{Domain, SockAddr, Socket, Type};

fn main() {
    let google_dns_server = "8.8.8.8:53";
    let google_dns_server_addr: SockAddr = google_dns_server
        .to_socket_addrs()
        .unwrap()
        .next()
        .unwrap()
        .into();

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None).expect("Failed to create socket");

    socket
        .connect(&google_dns_server_addr)
        .expect("Failed to connect to socket");

    let query = "example.com. IN A\n";

    socket.send(query.as_bytes()).expect("Failed to send query");

    let mut response = [MaybeUninit::<u8>::uninit(); 1024];
    let (_, source_address) = socket
        .recv_from(&mut response)
        .expect("Failed to receive response");

    assert_eq!(
        source_address.as_socket_ipv4().unwrap().to_string(),
        google_dns_server
    );
}
