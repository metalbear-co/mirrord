#![feature(result_option_inspect)]
use std::net::{SocketAddr, TcpListener, TcpStream};

/// Test that double binding on the same address:port combination fails the second time around.
fn main() {
    println!("test issue 1054: START");
    let address: SocketAddr = "1.1.1.1:80".parse().unwrap();
    let listener = TcpListener::bind(address).expect("Bind success for {address:#?}!");
    let cloned = listener
        .try_clone()
        .expect("Cloned listener {listener:#?}!");

    assert_eq!(listener.local_addr().unwrap(), address);
    assert_eq!(listener.local_addr().unwrap(), cloned.local_addr().unwrap());

    let connect_to = address.clone();
    let task = std::thread::spawn(move || {
        let _ = TcpStream::connect(connect_to).unwrap();
    });

    let (connection, _) = cloned
        .accept()
        .inspect_err(|fail| {
            eprintln!("Failed accept operation for cloned listener with {fail:#?}!")
        })
        .expect("Accept on cloned {cloned:#?}!");

    assert_ne!(connection.local_addr().unwrap(), address);

    task.join().unwrap();

    println!("test issue 1054: SUCCESS");
}
