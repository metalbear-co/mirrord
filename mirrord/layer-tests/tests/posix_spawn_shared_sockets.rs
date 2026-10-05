#![cfg(target_os = "linux")]

use std::net::SocketAddr;

mod common;
pub use common::*;

/// The application connects to a remote peer, then spawns two children that inherit the socket and
/// check that the layer in each of them reports the remote peer for it.
#[tokio::test]
async fn posix_spawn_children_receive_shared_socket_metadata() {
    let application = Application::PosixSpawnSharedSockets;
    let (mut test_process, mut intproxy) =
        application.start_process(Default::default(), None).await;

    let (uid, peer) = intproxy.recv_tcp_connect().await;
    assert_eq!(peer, "1.1.1.1:4567".parse::<SocketAddr>().unwrap());
    intproxy
        .send_tcp_connect_ok(uid, 0, peer, "1.2.3.4:6000".parse().unwrap())
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
