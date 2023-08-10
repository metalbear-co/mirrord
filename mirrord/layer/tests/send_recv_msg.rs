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
async fn send_recv_msg(
    #[values(Application::RustSendRecvMsg)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let mut config_path = config_dir.clone();
    config_path.push("send_recv_msg.json");
    let (mut test_process, layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], Some(config_path.to_str().unwrap()))
        .await;

    println!("Application started.");
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
                bytes: vec![1; 1],
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
        .assert_stdout_contains("test issue 1458: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1458: SUCCESS")
        .await;
}
