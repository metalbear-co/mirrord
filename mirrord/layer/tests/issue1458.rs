#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    outgoing::{SocketAddress, v2},
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1458](https://github.com/metalbear-co/mirrord/issues/1458) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1458(
    #[values(Application::RustIssue1458)] application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
            ],
            None,
        )
        .await;

    println!("Application started, preparing to resolve DNS.");

    let client_msg = intproxy.recv().await;
    let ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(v2::OutgoingConnectRequest {
        id,
        protocol: v2::OutgoingProtocol::Datagrams,
        address: SocketAddress::Ip(addr),
    })) = client_msg
    else {
        panic!("Invalid message received from layer: {client_msg:?}");
    };

    println!("connecting to address {addr:#?}");

    intproxy
        .send(DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Connect(
            v2::OutgoingConnectResponse {
                id,
                agent_local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                agent_peer_address: addr.into(),
            },
        )))
        .await;

    let client_msg = intproxy.recv().await;
    let ClientMessage::OutgoingV2(v2::ClientOutgoing::Data(v2::OutgoingData {
        data,
        id: received_id,
    })) = client_msg
    else {
        panic!("Invalid message received from layer: {client_msg:?}");
    };
    assert_eq!(received_id, id);
    assert_eq!(data.0.as_ref(), b"\0");

    intproxy
        .send(DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Data(
            v2::OutgoingData {
                id,
                data: vec![1].into(),
            },
        )))
        .await;

    intproxy
        .send(DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Close(
            v2::OutgoingClose { id },
        )))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1458: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1458: SUCCESS")
        .await;
}

/// Verify that we don't intercept UDP packets when `sendto` address' port is not `53`.
///
/// TODO(alex): When we fully implement proper UDP handling, this test will fail with some missing
/// message (just delete it), you've been warned.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1458_port_not_53(
    #[values(Application::RustIssue1458PortNot53)] application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
            ],
            None,
        )
        .await;

    println!("Application started, preparing to send UDP packet.");

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1458 port not 53: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1458 port not 53: SUCCESS")
        .await;
}
