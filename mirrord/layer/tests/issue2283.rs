#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{assert_matches::assert_matches, net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, SocketAddress,
    },
    ClientMessage, DaemonMessage,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2283](https://github.com/metalbear-co/mirrord/issues/2283) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2283(
    #[values(Application::NodeIssue2283)] application: Application,
    dylib_path: &Path,
    config_dir: &Path,
) {
    let mut config_path = config_dir.to_path_buf();
    config_path.push("outgoing_filter_local_not_existing_host.json");

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_REMOTE_DNS", "true")],
            Some(config_path.to_str().unwrap()),
        )
        .await;

    if cfg!(target_os = "macos") {
        intproxy
            .expect_file_open_for_reading("/etc/hostname", 2137)
            .await;
        intproxy.expect_only_file_read(2137).await;
        intproxy
            .answer_file_read("metalbear-hostname".as_bytes().to_vec())
            .await;
        intproxy.expect_file_close(2137).await;
    }

    let message = intproxy.recv().await;
    assert_matches!(message, ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node }) if node == "test-server");

    let address = "1.2.3.4:80".parse::<SocketAddr>().unwrap();

    intproxy
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
            DnsLookup(vec![LookupRecord {
                name: "test-server".to_string(),
                ip: address.ip(),
            }]),
        ))))
        .await;

    let message = intproxy.recv().await;
    assert_matches!(
        message,
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address}))
        if remote_address == SocketAddress::from(address)
    );

    let local_address = "2.3.4.5:9122".parse::<SocketAddr>().unwrap();
    intproxy
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: address.into(),
                local_address: local_address.into(),
            },
        ))))
        .await;
    intproxy
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes: vec![b'A'; 20],
            },
        ))))
        .await;
    intproxy
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(0)))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("connection established")
        .await;
    test_process
        .assert_stdout_contains("received 20 bytes of data")
        .await;
    test_process
        .assert_stdout_contains(
            "connection closed with no error, received 20 bytes, expected 20 bytes",
        )
        .await;
}
