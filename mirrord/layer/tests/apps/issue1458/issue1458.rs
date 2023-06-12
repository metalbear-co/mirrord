use std::net::{SocketAddr, UdpSocket};

/// Test that java [`netty`](https://netty.io/) can resolve DNS with UDP `send_to` and `recv_from`.
///
/// Using a rust sample to simplify testing this.
fn main() {
    println!("test issue 1458: START");

    let local_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let remote_addr: SocketAddr = "1.2.3.4:9876".parse().unwrap();

    let s = UdpSocket::bind(local_addr).unwrap();
    let amount = s.send_to(&vec![0; 1], remote_addr).unwrap();
    assert_eq!(amount, 1);

    let mut recv_buffer = vec![0; 1];
    let (amount, remote) = s.recv_from(&mut recv_buffer).unwrap();
    assert_eq!(amount, 1);
    assert_eq!(remote_addr, remote);
    assert_eq!(recv_buffer, vec![1; 1]);

    println!("test issue 1458: SUCCESS");
}
