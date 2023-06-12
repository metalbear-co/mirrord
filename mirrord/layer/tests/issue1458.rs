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

/// Verify that issue [#1458](https://github.com/metalbear-co/mirrord/issues/1458) is fixed.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[timeout(Duration::from_secs(60))]
async fn test_issue1458(
    #[values(Application::RustIssue1458)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, layer_connection) = application
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
    let mut connection = layer_connection.codec;

    let msg = connection.try_next().await.unwrap().unwrap();
    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };

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

    let msg = connection.try_next().await.unwrap().unwrap();

    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite { connection_id: 0, .. })) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
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
    test_process.assert_stdout_contains("test issue 1458: START");
    test_process.assert_stdout_contains("test issue 1458: SUCCESS");
}
