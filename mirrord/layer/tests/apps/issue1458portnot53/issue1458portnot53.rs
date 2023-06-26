use std::net::{SocketAddr, UdpSocket};

/// Counterpart test for issue 1458, where we check that mirrord doesn't intercept UDP when `sendto`
/// is used on a port other than `53`.
fn main() {
    println!("test issue 1458 port not 53: START");

    let unbound_local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let unbound_remote_addr: SocketAddr = "127.0.0.1:7777".parse().unwrap();

    let socket = UdpSocket::bind(unbound_local_addr).unwrap();
    let recv_socket = UdpSocket::bind(unbound_remote_addr).unwrap();
    println!("socket {socket:?} and recv {recv_socket:?} are bound");

    let local_addr = socket.local_addr().unwrap();
    let remote_addr = recv_socket.local_addr().unwrap();

    assert_eq!(
        local_addr,
        SocketAddr::new(unbound_local_addr.ip(), local_addr.port())
    );
    assert_eq!(remote_addr, unbound_remote_addr);

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
