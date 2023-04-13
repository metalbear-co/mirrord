#![feature(assert_matches)]
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

use common::*;
use futures::{SinkExt, TryStreamExt};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn outgoing_udp(dylib_path: &PathBuf) {
    let (mut test_process, layer_connection) = Application::RustOutgoingUdp
        .start_process_with_layer(dylib_path, Default::default())
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
                local_address: "0.0.0.0:4444".parse::<SocketAddr>().unwrap().into(),
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

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn outgoing_tcp(dylib_path: &PathBuf) {
    let (mut test_process, layer_connection) = Application::RustOutgoingTcp
        .start_process_with_layer(dylib_path, Default::default())
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
                local_address: "0.0.0.0:4444".parse::<SocketAddr>().unwrap().into(),
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
