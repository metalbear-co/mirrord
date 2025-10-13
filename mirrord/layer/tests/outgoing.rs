#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{
    collections::HashMap,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    path::Path,
    time::Duration,
};

use mirrord_protocol::{
    ClientMessage,
    outgoing::{SocketAddress, v2},
    uid::Uid,
};
use rstest::rstest;

mod common;

pub use common::*;
use tempfile::NamedTempFile;
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
#[timeout(Duration::from_secs(15))]
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
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(v2::OutgoingConnectRequest {
            id,
            protocol: v2::OutgoingProtocol::Datagrams,
            address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        intproxy
            .send(
                v2::OutgoingConnectResponse {
                    id,
                    agent_local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                    agent_peer_address: addr.into(),
                }
                .into(),
            )
            .await;

        let msg = intproxy.recv().await;
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Data(v2::OutgoingData {
            id: received_id,
            data,
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(received_id, id);
        intproxy.send(v2::OutgoingData { id, data }.into()).await;
        intproxy.send(v2::OutgoingClose { id }.into()).await;
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
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(v2::OutgoingConnectRequest {
            id,
            protocol: v2::OutgoingProtocol::Stream,
            address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        intproxy
            .send(
                v2::OutgoingConnectResponse {
                    id,
                    agent_local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                    agent_peer_address: addr.into(),
                }
                .into(),
            )
            .await;

        let msg = intproxy.recv().await;
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Data(v2::OutgoingData {
            id: received_id,
            data,
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(received_id, id);
        intproxy.send(v2::OutgoingData { id, data }.into()).await;
        intproxy.send(v2::OutgoingClose { id }.into()).await;
    }

    test_process.wait_assert_success().await;
}

/// See [`outgoing_tcp_logic`].
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
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
#[timeout(Duration::from_secs(15))]
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
#[timeout(Duration::from_secs(25))]
async fn outgoing_tcp_bound_socket(dylib_path: &Path) {
    let (mut test_process, mut intproxy) = Application::RustIssue2438
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let expected_peer_address = "1.1.1.1:4567".parse::<SocketAddr>().unwrap();

    // Test apps runs logic twice for 2 bind addresses.
    for _ in 0..2 {
        let msg = intproxy.recv().await;
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(v2::OutgoingConnectRequest {
            id,
            protocol: v2::OutgoingProtocol::Stream,
            address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, expected_peer_address);

        intproxy
            .send(
                v2::OutgoingConnectResponse {
                    id,
                    agent_local_address: "1.2.3.4:6000".parse::<SocketAddr>().unwrap().into(),
                    agent_peer_address: addr.into(),
                }
                .into(),
            )
            .await;

        let msg = intproxy.recv().await;
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Data(v2::OutgoingData {
            id: received_id,
            data,
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(received_id, id);
        intproxy.send(v2::OutgoingData { id, data }.into()).await;
        intproxy.send(v2::OutgoingClose { id }.into()).await;
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
        let (id, addr) = match intproxy.recv().await {
            ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(
                v2::OutgoingConnectRequest {
                    id,
                    protocol: v2::OutgoingProtocol::Stream,
                    address: SocketAddress::Ip(addr),
                },
            )) => (id, addr),
            ClientMessage::OutgoingV2(v2::ClientOutgoing::Close(v2::OutgoingClose { .. })) => {
                continue;
            }
            other => panic!("Received an unexpected message from the test app: {other:?}"),
        };

        assert!(
            peers[1..].contains(&addr),
            "Receved a connect request for an unexpected host: {addr}"
        );

        got_connect_requests += 1;

        intproxy
            .send(
                v2::OutgoingConnectResponse {
                    id,
                    agent_local_address: SocketAddress::Ip(SocketAddr::new(
                        Ipv4Addr::LOCALHOST.into(),
                        0,
                    )),
                    agent_peer_address: SocketAddress::Ip(addr),
                }
                .into(),
            )
            .await;
    }

    test_process.wait_assert_success().await;
}

/// Verifies that `experimental.non_blocking_tcp_connect` allows for making outgoing TCP connections
/// in parallel.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn outgoing_tcp_non_blocking_connect(dylib_path: &Path) {
    let config = serde_json::json!({
        "experimental": {
            "non_blocking_tcp_connect": true
        }
    })
    .to_string()
    .into_bytes();
    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
    config_file.as_file_mut().write_all(&config).unwrap();

    let (mut test_process, mut intproxy) = Application::RustOutgoingTcp
        .start_process_with_layer(dylib_path, vec![], Some(config_file.path()))
        .await;

    let mut connections = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .map(|addr| (addr, None))
        .collect::<HashMap<_, Option<Uid>>>();
    while connections.values().all(Option::is_some).not() {
        let msg = intproxy.recv().await;
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(v2::OutgoingConnectRequest {
            id,
            protocol: v2::OutgoingProtocol::Stream,
            address: SocketAddress::Ip(addr),
        })) = msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };

        let duplicate = connections.get_mut(&addr).unwrap().replace(id);
        if duplicate.is_some() {
            panic!("Duplicate destination address {addr}");
        }

        println!("Received connect request for {addr} with id {id}");
    }

    for (addr, id) in &connections {
        let id = id.unwrap();
        intproxy
            .send(
                v2::OutgoingConnectResponse {
                    id,
                    agent_local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                    agent_peer_address: (*addr).into(),
                }
                .into(),
            )
            .await;
    }

    while connections.is_empty().not() {
        let msg = intproxy.recv().await;
        let ClientMessage::OutgoingV2(v2::ClientOutgoing::Data(v2::OutgoingData { id, data })) =
            msg
        else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        println!("Received data in connection {id}");

        let prev_len = connections.len();
        connections.retain(|_, known_id| id != known_id.unwrap());
        if connections.len() != prev_len {
            panic!("Unexpected outgoing connection id {id}");
        }

        intproxy.send(v2::OutgoingData { id, data }.into()).await;
        intproxy.send(v2::OutgoingClose { id }.into()).await;
    }

    test_process.wait_assert_success().await;
}
