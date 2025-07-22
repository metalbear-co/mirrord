use std::net::{SocketAddr, UdpSocket};

/// Test that java [`netty`](https://netty.io/) can resolve DNS with UDP `send_to` and `recv_from`.
///
/// Using a rust sample to simplify testing this.
fn main() {
    println!("test issue 1458: START");

    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let remote_addr: SocketAddr = "1.2.3.4:53".parse().unwrap();

    let socket = UdpSocket::bind(local_addr).unwrap();

    // mirrord should intercept this
    let amount = socket.send_to(&vec![0; 1], remote_addr).unwrap();
    assert_eq!(amount, 1);

    let mut recv_buffer = vec![0; 1];
    let (amount, remote) = socket.recv_from(&mut recv_buffer).unwrap();
    assert_eq!(amount, 1);
    assert_eq!(remote_addr, remote);
    assert_eq!(recv_buffer, vec![1; 1]);

    println!("test issue 1458: SUCCESS");
}
