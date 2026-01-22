#![cfg(target_os = "macos")]
#![feature(assert_matches)]

use rstest::rstest;

mod common;

use std::{path::Path, time::Duration};

pub use common::*;
use mirrord_protocol::outgoing::{DaemonConnectV2, LayerConnectV2, SocketAddress};
//use mirrord_protocol::tcp::{LayerTcpSteal, StealType};

/// Test outgoing TCP with BSD connectx(2).
/// 1. Bind 0.0.0.0:23333
/// 2. Tcp outgoing connect 127.0.0.1:80
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_connectx(
    #[values(Application::Connectx)] application: Application,
    dylib_path: &Path,
) {
    use mirrord_protocol::{
        ClientMessage, DaemonMessage,
        outgoing::{
            DaemonConnect, LayerWrite,
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        },
    };

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
        uid,
        remote_address,
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(
            DaemonConnectV2 {
                uid,
                connect: Ok(DaemonConnect {
                    connection_id: 0,
                    remote_address,
                    local_address: SocketAddress::Ip("127.0.0.1:51070".parse().unwrap()),
                }),
            },
        )))
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
    assert_eq!(b"hello\n", bytes.to_vec().as_slice());

    test_process
        .child
        .kill()
        .await
        .expect("failed to kill the app");
}
