#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

mod common;

use std::{io::Write, net::SocketAddr, ops::Not, path::Path, time::Duration};

pub use common::*;
use mirrord_protocol::{
    ClientMessage, ConnectionId, DaemonMessage,
    outgoing::{
        DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite, SocketAddress,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
};
use rstest::rstest;
use tempfile::NamedTempFile;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn outgoing_tcp(dylib_path: &Path) {
    let config = serde_json::json!({
        "experimental": {
            "non_blocking_tcp_connect": true
        }
    });
    let mut config_file = NamedTempFile::with_suffix("json").unwrap();
    config_file
        .as_file_mut()
        .write_all(config.to_string().as_bytes())
        .unwrap();

    let (mut test_process, mut intproxy) = Application::RustOutgoingTcp { non_blocking: true }
        .start_process_with_layer(dylib_path, vec![], Some(config_file.path()))
        .await;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    let mut received_connects: Vec<(SocketAddr, ConnectionId)> = Vec::with_capacity(peers.len());
    let mut received_data: Vec<ConnectionId> = Vec::with_capacity(peers.len());

    loop {
        if received_connects.len() == peers.len() && received_data.len() == peers.len() {
            break;
        }

        let msg = intproxy.recv().await;
        match msg {
            ClientMessage::TcpOutgoing(message) => match message {
                LayerTcpOutgoing::Connect(LayerConnect {
                    remote_address: SocketAddress::Ip(addr),
                }) => {
                    if peers.contains(&addr).not() {
                        panic!("unexpected connect request to {addr}");
                    }
                    if received_connects
                        .iter()
                        .any(|(prev_addr, _)| *prev_addr == addr)
                    {
                        panic!("duplicate connect request to {addr}");
                    }
                    let connection_id = received_connects.len() as ConnectionId;
                    received_connects.push((addr, connection_id));
                    intproxy
                        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                            DaemonConnect {
                                connection_id,
                                remote_address: addr.into(),
                                local_address: RUST_OUTGOING_LOCAL
                                    .parse::<SocketAddr>()
                                    .unwrap()
                                    .into(),
                            },
                        ))))
                        .await;
                }
                LayerTcpOutgoing::Write(LayerWrite {
                    connection_id,
                    bytes,
                }) => {
                    if received_connects
                        .iter()
                        .any(|(_, id)| *id == connection_id)
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
