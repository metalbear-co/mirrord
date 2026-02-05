#![feature(assert_matches)]
use std::{assert_matches::assert_matches, net::Ipv4Addr, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    tcp::{DaemonTcp, LayerTcpSteal, NewTcpConnectionV1, StealType, TcpClose, TcpData},
};
use rstest::rstest;

mod common;
pub use common::*;

/// Tests that sockets are only dropped when all fds pointing to them
/// are closed, not just one.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn double_listen(dylib_path: &Path) {
    let application = Application::DoubleListen;
    let port = application.get_app_port();
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            Default::default(),
            Some(&config_dir().join("steal.json")),
        )
        .await;

    assert_matches!(intproxy.recv().await,
        ClientMessage::TcpSteal(
            LayerTcpSteal::PortSubscribe(StealType::All(sub))
        ) if sub == port
    );

    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
            port,
        ))))
        .await;

    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::NewConnectionV1(
            NewTcpConnectionV1 {
                connection_id: 0,
                remote_address: "127.0.0.1".parse().unwrap(),
                destination_port: port,
                source_port: 10000,
                local_address: Ipv4Addr::LOCALHOST.into(),
            },
        )))
        .await;

    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
            connection_id: 0,
            bytes: "some data".into(),
        })))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::TcpSteal(LayerTcpSteal::Data(
            TcpData {
                connection_id: 0,
                bytes
            }
        )) if &**bytes == b"some data"
    );

    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
            connection_id: 0,
        })))
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;

    test_process
        .assert_stdout_contains("First listen() succeeded\n")
        .await;
    test_process
        .assert_stdout_contains("Second listen() succeeded\n")
        .await;
    test_process
        .assert_stdout_contains("Connection accepted, starting echo server\n")
        .await;
}
