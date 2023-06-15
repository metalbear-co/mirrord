use std::net::{SocketAddr, UdpSocket};

/// Counterpart test for issue 1458, where we check that mirrord doesn't intercept UDP when `sendto`
/// is used on a port other than `53`.
fn main() {
    println!("test issue 1458 port not 53: START");

    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let remote_addr: SocketAddr = "127.0.0.1:7777".parse().unwrap();

    let socket = UdpSocket::bind(local_addr).unwrap();
    let recv_socket = UdpSocket::bind(remote_addr).unwrap();

    let recv_thread = std::thread::spawn(move || {
        let mut recv_buffer = vec![0; 1];
        let (amount, remote) = recv_socket.recv_from(&mut recv_buffer).unwrap();
        assert_eq!(amount, 1);
        assert_ne!(remote_addr, remote);
        assert_eq!(local_addr, remote);
        assert_eq!(recv_buffer, vec![0; 1]);
    });

    // mirrord should NOT intercept this
    let amount = socket.send_to(&vec![0; 1], remote_addr).unwrap();
    assert_eq!(amount, 1);

    recv_thread.join().unwrap();

    println!("test issue 1458 port not 53: SUCCESS");
}
