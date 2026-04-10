#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

mod common;

use std::{net::SocketAddr, ops::Not, time::Duration};

pub use common::*;
use mirrord_protocol::{
    ClientMessage, ConnectionId, DaemonMessage,
    outgoing::{
        DaemonRead, LayerClose, LayerWrite,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
    uid::Uid,
};
use rstest::rstest;

/// Verifies that a single-threaded async application can concurrently start multiple outgoing
/// connections (non-blocking TCP connect is always enabled).
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn issue_2744_non_blocking_outgoing_tcp() {
    let (mut test_process, mut intproxy) = Application::RustOutgoingTcp
        .start_process(vec![], None)
        .await;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    // Verify that intproxy sends concurrent connect requests.
    // The test app uses a single-threaded runtime, so this is enough to check whether the connect
    // emulation really is non-blocking.
    let mut received_connects: Vec<(SocketAddr, ConnectionId, Uid)> =
        Vec::with_capacity(peers.len());
    while received_connects.len() < peers.len() {
        let (uid, addr) = intproxy.recv_tcp_connect().await;
        if peers.contains(&addr).not() {
            panic!("unexpected connect request to {addr}");
        }
        if received_connects
            .iter()
            .any(|(prev_addr, _, _)| *prev_addr == addr)
        {
            panic!("duplicate connect request to {addr}");
        }
        let connection_id = received_connects.len() as ConnectionId;
        received_connects.push((addr, connection_id, uid));
    }
    for (addr, id, uid) in &received_connects {
        intproxy
            .send_tcp_connect_ok(*uid, *id, *addr, RUST_OUTGOING_LOCAL.parse().unwrap())
            .await;
    }

    let mut received_data: Vec<ConnectionId> = Vec::with_capacity(peers.len());
    while received_data.len() < peers.len() {
        let msg = intproxy.recv().await;
        match msg {
            ClientMessage::TcpOutgoing(message) => match message {
                LayerTcpOutgoing::Write(LayerWrite {
                    connection_id,
                    bytes,
                }) => {
                    if received_connects
                        .iter()
                        .any(|(_, id, _)| *id == connection_id)
                        .not()
                    {
                        panic!("unexpected write to {connection_id}");
                    }
                    if received_data.contains(&connection_id) {
                        panic!("duplicate write to {connection_id}");
                    }
                    received_data.push(connection_id);

                    intproxy
                        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                            DaemonRead {
                                connection_id,
                                bytes,
                            },
                        ))))
                        .await;
                    intproxy
                        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(
                            connection_id,
                        )))
                        .await;
                }
                LayerTcpOutgoing::Close(LayerClose { connection_id }) => {
                    if received_data.contains(&connection_id).not() {
                        panic!("unexpected close of {connection_id}");
                    }
                }
                other => panic!("unexpected client message {other:?}"),
            },
            other => panic!("unexpected client message {other:?}"),
        }
    }

    test_process.wait_assert_success().await;
}
