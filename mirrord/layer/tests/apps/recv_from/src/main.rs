use std::{
    mem::MaybeUninit,
    net::{SocketAddr, ToSocketAddrs},
};

use socket2::{Domain, Socket, Type};

fn main() {
    let google_dns_server = "8.8.8.8:53";

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None).expect("Failed to create socket");

    let query = "example.com. IN A\n";
    let server_address: SocketAddr = google_dns_server.to_socket_addrs().unwrap().next().unwrap();

    socket
        .send_to(query.as_bytes(), &server_address.into())
        .expect("Failed to send query");

    let mut response = [MaybeUninit::<u8>::uninit(); 1024];
    let (_, source_address) = socket
        .recv_from(&mut response)
        .expect("Failed to receive response");

    if let Some(addr) = source_address.as_socket_ipv4() {
        assert_eq!(addr.to_string(), google_dns_server);
    }
}
