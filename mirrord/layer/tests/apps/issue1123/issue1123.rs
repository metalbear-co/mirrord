use std::net::{SocketAddr, TcpListener};

/// Test that double binding on the same address:port combination fails the second time around.
fn main() {
    println!("test issue 1123: START");
    let localhost: SocketAddr = "127.0.0.1:41222".parse().unwrap();
    let _listener_1 = TcpListener::bind(localhost).expect("first bind failed");
    TcpListener::bind(localhost).expect_err("second bind did not fail!");
    println!("test issue 1123: SUCCESS");
}
