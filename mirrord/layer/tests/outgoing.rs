#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessageV1,
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
///
/// # Ignored
/// This test is ignored due to a bug - `recv_from` call returns an invalid remote peer address.
#[ignore]
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_udp(dylib_path: &PathBuf) {
    let (mut test_process, mut intproxy) = Application::RustOutgoingUdp
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let msg = intproxy.recv().await;
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
            remote_address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        intproxy
            .send(DaemonMessageV1::UdpOutgoing(DaemonUdpOutgoing::Connect(
                Ok(DaemonConnect {
                    connection_id: 0,
                    remote_address: addr.into(),
                    local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                }),
            )))
            .await;

        let msg = intproxy.recv().await;
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite {
            connection_id: 0,
            bytes,
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        intproxy
            .send(DaemonMessageV1::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 0,
                    bytes,
                },
            ))))
            .await;
        intproxy
            .send(DaemonMessageV1::UdpOutgoing(DaemonUdpOutgoing::Close(0)))
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
async fn outgoing_tcp_logic(with_config: Option<&str>, dylib_path: &PathBuf, config_dir: &PathBuf) {
    let config = with_config.map(|config| {
        let mut config_path = config_dir.clone();
        config_path.push(config);
        config_path
    });
    let config = config.as_ref().map(|path_buf| path_buf.to_str().unwrap());

    let (mut test_process, mut intproxy) = Application::RustOutgoingTcp
        .start_process_with_layer(dylib_path, vec![], config)
        .await;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let msg = intproxy.recv().await;
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        intproxy
            .send(DaemonMessageV1::TcpOutgoing(DaemonTcpOutgoing::Connect(
                Ok(DaemonConnect {
                    connection_id: 0,
                    remote_address: addr.into(),
                    local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                }),
            )))
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
            .send(DaemonMessageV1::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 0,
                    bytes,
                },
            ))))
            .await;
        intproxy
            .send(DaemonMessageV1::TcpOutgoing(DaemonTcpOutgoing::Close(0)))
            .await;
    }

    test_process.wait_assert_success().await;
}

/// See [`outgoing_tcp_logic`].
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_tcp(
    #[values(None, Some("outgoing_filter.json"))] with_config: Option<&str>,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    outgoing_tcp_logic(with_config, dylib_path, config_dir).await;
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
#[timeout(Duration::from_secs(10))]
#[should_panic]
async fn outgoing_tcp_from_the_local_app_broken(
    #[values(
        Some("outgoing_filter_local.json"),
        Some("outgoing_filter_remote_incomplete.json")
    )]
    with_config: Option<&str>,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    outgoing_tcp_logic(with_config, dylib_path, config_dir).await;
}
