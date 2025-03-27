#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};

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
use tokio::net::TcpListener;

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
#[timeout(Duration::from_secs(10))]
async fn outgoing_udp(dylib_path: &Path) {
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
            .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 0,
                    remote_address: addr.into(),
                    local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                },
            ))))
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
            .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 0,
                    bytes,
                },
            ))))
            .await;
        intproxy
            .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(0)))
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
async fn outgoing_tcp_logic(with_config: Option<&str>, dylib_path: &Path, config_dir: &Path) {
    let config = with_config.map(|config| {
        let mut config_path = config_dir.to_path_buf();
        config_path.push(config);
        config_path
    });

    let (mut test_process, mut intproxy) = Application::RustOutgoingTcp
        .start_process_with_layer(dylib_path, vec![], config.as_deref())
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
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 0,
                    remote_address: addr.into(),
                    local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                },
            ))))
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
#[timeout(Duration::from_secs(10))]
async fn outgoing_tcp(
    #[values(None, Some("outgoing_filter.json"))] with_config: Option<&str>,
    dylib_path: &Path,
    config_dir: &Path,
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
    dylib_path: &Path,
    config_dir: &Path,
) {
    outgoing_tcp_logic(with_config, dylib_path, config_dir).await;
}

/// Tests that outgoing connections are properly handled on sockets that were bound by the user
/// application.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn outgoing_tcp_bound_socket(dylib_path: &Path) {
    let (mut test_process, mut intproxy) = Application::RustIssue2438
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let expected_peer_address = "1.1.1.1:4567".parse::<SocketAddr>().unwrap();

    // Test apps runs logic twice for 2 bind addresses.
    for _ in 0..2 {
        let msg = intproxy.recv().await;
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, expected_peer_address);

        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 0,
                    remote_address: addr.into(),
                    local_address: "1.2.3.4:6000".parse::<SocketAddr>().unwrap().into(),
                },
            ))))
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

/// Verifies that issue <https://github.com/metalbear-co/mirrord/issues/3212> is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn outgoing_tcp_high_port(dylib_path: &Path) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _conn = listener.accept().await;
        std::future::pending::<()>().await;
    });
    assert!(
        listener_addr.port() >= 1000,
        "should be a high port: {listener_addr}"
    );

    let peers = [
        // Should be bypassed as a debugger port.
        // The listener will accept the connection in the background task we spawn above.
        listener_addr,
        // This should be remote, even though it's localhost,
        // and the port is in the debugger ports range.
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 50000),
        // This should be remote, even though it's localhost,
        // and the port is in the debugger ports range.
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 50001),
        // This should be remote, because the `outgoing.ignore_localhost` is `false` by default.
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80),
    ];

    let (mut test_process, mut intproxy) = Application::NodeMakeConnections
        .start_process_with_layer(
            dylib_path,
            vec![
                ("TO_HOSTS", &peers.map(|p| p.to_string()).join(",")),
                ("MIRRORD_IGNORE_DEBUGGER_PORTS", "1000-65535"),
            ],
            None,
        )
        .await;

    let mut got_connect_requests = 0;

    while got_connect_requests < 3 {
        let addr = match intproxy.recv().await {
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                remote_address: SocketAddress::Ip(addr),
            })) => addr,
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(..)) => continue,
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(..)) => continue,
            other => panic!("Received an unexpected message from the test app: {other:?}"),
        };

        assert!(
            addr == peers[1] || addr == peers[2] || addr == peers[3],
            "Receved a connect request for an unexpected host: {addr}"
        );

        got_connect_requests += 1;

        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: got_connect_requests,
                    remote_address: SocketAddress::Ip(addr),
                    local_address: SocketAddress::Ip(SocketAddr::new(
                        Ipv4Addr::LOCALHOST.into(),
                        0,
                    )),
                },
            ))))
            .await;
    }

    test_process.wait_assert_success().await;
}
