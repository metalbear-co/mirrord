use std::net::{SocketAddr, TcpListener};

/// Test that double binding on the same address:port combination fails the second time around.
fn main() {
    println!("test issue 1123: START");
    let localhost: SocketAddr = "127.0.0.1:41222".parse().unwrap();
    let _ = TcpListener::bind(localhost)
        .map(|_| TcpListener::bind(localhost).expect_err("Second bind should have failed!"))
        .expect("First bind success!");
    println!("test issue 1123: SUCCESS");
}
