#![feature(assert_matches)]
#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessage,
};
use rstest::rstest;

mod common;

pub use common::*;
use futures::{SinkExt, TryStreamExt};

// TODO: add a test for when DNS lookup is unsuccessful, to make sure the layer returns a valid
//      error to the user application.

/// Test outgoing UDP.
/// Application, for each remote peer in [`RUST_OUTGOING_PEERS`]:
/// 1. Opens a UDP port at [`RUST_OUTGOING_LOCAL`]
/// 2. Connects to the remote peer
/// 3. Sends some data
/// 4. Expects the peer to send the same data back
///
/// # Ignored
/// This test is ignored due to a bug - `recv_from` call returns an invalid remote peer address.
#[ignore]
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_udp(dylib_path: &PathBuf) {
    let (mut test_process, layer_connection) = Application::RustOutgoingUdp
        .start_process_with_layer(dylib_path, vec![], None)
        .await;
    let mut conn = layer_connection.codec;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            },
        ))))
        .await
        .unwrap();

        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite { connection_id: 0, bytes })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes,
            },
        ))))
        .await
        .unwrap();
        conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(0)))
            .await
            .unwrap();
    }

    test_process.wait_assert_success().await;
}

/// Test outgoing TCP.
/// Application, for each remote peer in [`RUST_OUTGOING_PEERS`]:
/// 1. Opens a TCP port at [`RUST_OUTGOING_LOCAL`]
/// 2. Connects to the remote peer
/// 3. Sends some data
/// 4. Expects the peer to send the same data back
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_tcp(dylib_path: &PathBuf) {
    let (mut test_process, layer_connection) = Application::RustOutgoingTcp
        .start_process_with_layer(dylib_path, vec![], None)
        .await;
    let mut conn = layer_connection.codec;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        conn.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            },
        ))))
        .await
        .unwrap();

        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite { connection_id: 0, bytes })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        conn.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes,
            },
        ))))
        .await
        .unwrap();
        conn.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(0)))
            .await
            .unwrap();
    }

    test_process.wait_assert_success().await;
}
