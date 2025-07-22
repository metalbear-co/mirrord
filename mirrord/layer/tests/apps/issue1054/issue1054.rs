use std::{
    io::Read,
    net::{SocketAddr, TcpListener},
};

/// Tests that cloned TCP listener works fine.
fn main() {
    println!("test issue 1054: START");
    let address: SocketAddr = "0.0.0.0:80".parse().expect("failed to bind");
    let listener = TcpListener::bind(address).unwrap();
    let cloned = listener
        .try_clone()
        .expect("failed to clone listneer");

    assert_eq!(listener.local_addr().unwrap(), address);
    assert_eq!(listener.local_addr().unwrap(), cloned.local_addr().unwrap());

    let (mut connection, _) = cloned
        .accept()
        .inspect_err(|fail| {
            eprintln!("Failed accept operation for cloned listener with {fail:#?}!")
        })
        .expect("failed to accept");

    // Note - if we just open the connection and immediately close it on the other side,
    // intproxy may clean up its metadata before the layer has a chance to fetch it.
    // This fails the `local_addr` assert below. Hence, we send some data first.
    let mut buf = [0_u8; 16];
    let bytes_read = connection.read(&mut buf).expect("failed to read");
    assert_eq!(bytes_read, 5);
    assert_eq!(&buf[..bytes_read], b"hello");

    assert_eq!(
        connection.local_addr().unwrap(),
        "1.1.1.1:80".parse().unwrap()
    );

    println!("test issue 1054: SUCCESS");
}
