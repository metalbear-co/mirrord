#![feature(assert_matches)]

mod common;

use std::{path::Path, time::Duration};

pub use common::*;
pub use rstest::rstest;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn issue3899(#[values(Application::CIssue3899)] application: Application, dylib_path: &Path) {
    use mirrord_protocol::{
        ClientMessage,
        outgoing::{LayerWrite, tcp::LayerTcpOutgoing},
    };

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let (uid, addr) = intproxy.recv_tcp_connect().await;
    assert_eq!(addr, "127.0.0.1:8080".parse().unwrap());
    intproxy
        .send_tcp_connect_ok(uid, 0, addr, "127.0.0.1:51070".parse().unwrap())
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
        connection_id,
        bytes,
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    assert_eq!(0, connection_id);
    assert_eq!(b"before spawn\n", bytes.to_vec().as_slice());

    let msg = intproxy.recv().await;
    let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
        connection_id,
        bytes,
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    assert_eq!(0, connection_id);
    assert_eq!(b"after spawn\n", bytes.to_vec().as_slice());

    test_process
        .child
        .kill()
        .await
        .expect("failed to kill the app");
}
