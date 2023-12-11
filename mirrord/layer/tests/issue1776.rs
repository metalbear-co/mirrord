#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{net::SocketAddr, path::PathBuf, time::Duration};

use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessageV1,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1776](https://github.com/metalbear-co/mirrord/issues/1776) is fixed.
///
/// We test this with `outgoing.udp = false`, as we're just trying to resolve DNS, and not full UDP
/// outgoing traffic.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1776(
    #[values(Application::RustIssue1776)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let mut config_path = config_dir.clone();
    config_path.push("issue1776.json");
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], Some(config_path.to_str().unwrap()))
        .await;

    println!("Application started, preparing to resolve DNS with sendmsg/recvmsg.");

    let client_msg = intproxy.recv().await;
    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
        remote_address: SocketAddress::Ip(addr),
    })) = client_msg
    else {
        panic!("Invalid message received from layer: {client_msg:?}");
    };

    println!("connecting to address {addr:#?}");

    intproxy
        .send(DaemonMessageV1::UdpOutgoing(DaemonUdpOutgoing::Connect(
            Ok(DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            }),
        )))
        .await;

    let client_msg = intproxy.recv().await;
    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite {
        connection_id: 0, ..
    })) = client_msg
    else {
        panic!("Invalid message received from layer: {client_msg:?}");
    };

    intproxy
        .send(DaemonMessageV1::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes: vec![0; 4],
            },
        ))))
        .await;

    intproxy
        .send(DaemonMessageV1::UdpOutgoing(DaemonUdpOutgoing::Close(0)))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1776: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1776: SUCCESS")
        .await;
}

/// Verify that we don't intercept UDP packets when `sendmsg` address' port is not `53`.
///
/// TODO(alex): When we fully implement proper UDP handling, this test will fail with some missing
/// message (just delete it), you've been warned.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1776_port_not_53(
    #[values(Application::RustIssue1776PortNot53)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let mut config_path = config_dir.clone();
    config_path.push("issue1776.json");
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], Some(config_path.to_str().unwrap()))
        .await;

    println!("Application started, preparing to send UDP packet.");

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1776 port not 53: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1776 port not 53: SUCCESS")
        .await;
}
