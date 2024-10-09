#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{collections::HashMap, net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        DaemonConnect, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, ConnectionId, DaemonMessage,
};
use rstest::rstest;

mod common;

pub use common::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnState {
    None,
    Established(ConnectionId),
    Shutdown,
}

/// Verify that issue [#1898](https://github.com/metalbear-co/mirrord/issues/1898) is fixed
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1898(
    #[values(Application::RustIssue1898)] application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let local_address = SocketAddress::Ip("3.4.5.6:9999".parse().unwrap());
    let mut addresses: HashMap<SocketAddr, ConnState> = [
        ("1.2.3.4:80".parse().unwrap(), ConnState::None),
        ("2.3.4.5:80".parse().unwrap(), ConnState::None),
    ]
    .into();
    let mut next_conn_id: ConnectionId = 0;

    loop {
        let message = intproxy.recv().await;

        match message {
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                remote_address,
            })) => {
                let ip = match remote_address {
                    SocketAddress::Ip(ip) => ip,
                    SocketAddress::Unix(unix) => {
                        panic!("unexpected connect request from intproxy: {unix:?}")
                    }
                };

                let conn_state = addresses
                    .get_mut(&ip)
                    .unwrap_or_else(|| panic!("unexpected connect request from intproxy: {ip}"));

                let ConnState::None = conn_state else {
                    panic!("received duplicate connect request to {ip}");
                };
                *conn_state = ConnState::Established(next_conn_id);

                eprintln!("Received connection request for remote address {ip}");
                intproxy
                    .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                        DaemonConnect {
                            connection_id: next_conn_id,
                            remote_address: SocketAddress::Ip(ip),
                            local_address: local_address.clone(),
                        },
                    ))))
                    .await;
                eprintln!("Responded to connection request for remote address {ip}, connection_id {next_conn_id}");

                next_conn_id += 1;
            }

            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
            })) => {
                let conn_state = addresses
                    .values_mut()
                    .find(|conn_state| **conn_state == ConnState::Established(connection_id))
                    .unwrap_or_else(|| {
                        panic!(
                            "unexpected write request from intproxy, connection_id {connection_id}"
                        )
                    });

                if bytes.is_empty() {
                    eprintln!("intproxy shut down connection {connection_id}");
                    *conn_state = ConnState::Shutdown;

                    if addresses
                        .values()
                        .all(|conn_state| matches!(conn_state, ConnState::Shutdown))
                    {
                        eprintln!("intproxy shut down all connections");
                        break;
                    }
                } else {
                    eprintln!("intproxy sent data in connection {connection_id}");
                }
            }

            other => panic!("unexpected message from intproxy: {other:?}"),
        }
    }

    test_process.wait().await;
    test_process.assert_stdout_contains("SUCCESS").await;

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
