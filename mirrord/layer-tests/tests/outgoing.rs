#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    outgoing::{
        DaemonRead, LayerWrite,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
};
use rstest::rstest;

mod common;

pub use common::*;

// TODO: add a test for when DNS lookup is unsuccessful, to make sure the layer returns a valid
//      error to the user application.

/// Test outgoing UDP.
/// Application, for each remote peer in [`RUST_OUTGOING_PEERS`]:
/// 1. Opens a UDP port at [`RUST_OUTGOING_LOCAL`]
/// 2. Connects to the remote peer
/// 3. Sends some data
/// 4. Expects the peer to send the same data back
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn outgoing_udp() {
    let (mut test_process, mut intproxy) = Application::RustOutgoingUdp
        .start_process_with_layer(vec![], None)
        .await;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for (connection_id, peer) in peers.into_iter().enumerate() {
        let connection_id = connection_id as u64;

        let (uid, addr) = intproxy.recv_udp_connect().await;
        assert_eq!(addr, peer);
        intproxy
            .send_udp_connect_ok(
                uid,
                connection_id,
                addr,
                RUST_OUTGOING_LOCAL.parse().unwrap(),
            )
            .await;

        let msg = intproxy.recv().await;
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite {
            connection_id: response_connection_id,
            bytes,
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };

        assert_eq!(response_connection_id, connection_id);

        intproxy
            .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id,
                    bytes,
                },
            ))))
            .await;

        intproxy
            .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(
                connection_id,
            )))
            .await;
    }

    test_process.wait_assert_success().await;
}

/// Test outgoing TCP.
/// Application, for each remote peer in [`RUST_OUTGOING_PEERS`]:
/// 1. Opens a TCP port at [`RUST_OUTGOING_LOCAL`]
/// 2. Connects to the remote peer
/// 3. Sends some data
/// 4. Expects the peer to send the same data back
async fn outgoing_tcp_logic(with_config: Option<&str>, config_dir: &Path) {
    let config = with_config.map(|config| {
        let mut config_path = config_dir.to_path_buf();
        config_path.push(config);
        config_path
    });

    let (mut test_process, mut intproxy) = Application::RustOutgoingTcp {
        non_blocking: false,
    }
    .start_process_with_layer(vec![], config.as_deref())
    .await;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let (uid, addr) = intproxy.recv_tcp_connect().await;
        assert_eq!(addr, peer);
        intproxy
            .send_tcp_connect_ok(uid, 0, addr, RUST_OUTGOING_LOCAL.parse().unwrap())
            .await;

        let msg = intproxy.recv().await;
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 0,
            bytes,
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 0,
                    bytes,
                },
            ))))
            .await;
        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(0)))
            .await;
    }

    test_process.wait_assert_success().await;
}

/// See [`outgoing_tcp_logic`].
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn outgoing_tcp(
    #[values(None, Some("outgoing_filter.json"))] with_config: Option<&str>,
    config_dir: &Path,
) {
    outgoing_tcp_logic(with_config, config_dir).await;
}

/// 1. Tries to go through the [`outgoing_tcp_logic`] flow, except that outgoing traffic is
/// configured to go from the local app, which means that the daemon handler won't be triggered,
/// thus this send will hang (with the whole test hanging) and crashing on timeout, verifying that,
/// indeed, the connection was not relayed to the agent.
///
/// 2. Similar to the [`outgoing_tcp`] test, but we don't add the `remote` address `3.3.3.3` to the
/// list, thus it should go through local, and hang.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
#[should_panic]
async fn outgoing_tcp_from_the_local_app_broken(
    #[values(
        Some("outgoing_filter_local.json"),
        Some("outgoing_filter_remote_incomplete.json")
    )]
    with_config: Option<&str>,
    config_dir: &Path,
) {
    outgoing_tcp_logic(with_config, config_dir).await;
}
