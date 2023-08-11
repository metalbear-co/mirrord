#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{net::SocketAddr, path::PathBuf, time::Duration};

use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessage,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Start an application (and load the layer into it) that listens on a port that is configured to
/// be ignored, and verify that no messages are sent to the agent.
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
    let (mut test_process, layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], Some(config_path.to_str().unwrap()))
        .await;

    println!("Application started, preparing to resolve DNS with sendmsg/recvmsg.");
    let mut connection = layer_connection.codec;

    let client_msg = connection.try_next().await.unwrap().unwrap();
    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = client_msg else {
            panic!("Invalid message received from layer: {client_msg:?}");
    };

    println!("connecting to address {addr:#?}");

    connection
        .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            },
        ))))
        .await
        .unwrap();

    let client_msg = connection.try_next().await.unwrap().unwrap();
    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite { connection_id: 0, .. })) = client_msg else {
        panic!("Invalid message received from layer: {client_msg:?}");
    };

    connection
        .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes: vec![0; 4],
            },
        ))))
        .await
        .unwrap();

    connection
        .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(0)))
        .await
        .unwrap();

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1776: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1776: SUCCESS")
        .await;
}

/// Verify that we don't intercept UDP packets when `sendto` address' port is not `53`.
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
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], Some(config_path.to_str().unwrap()))
        .await;

    println!("Application started, preparing to send UDP packet.");

    assert!(layer_connection.codec.try_next().await.unwrap().is_none());

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1776 port not 53: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1776 port not 53: SUCCESS")
        .await;
}
