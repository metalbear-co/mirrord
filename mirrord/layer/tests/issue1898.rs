#![cfg(target_os = "macos")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{assert_matches::assert_matches, net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        DaemonConnect, LayerConnect, SocketAddress,
    },
    ClientMessage, DaemonMessage,
};
use rstest::rstest;

mod common;

pub use common::*;

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

    let address = "1.2.3.4:80".parse::<SocketAddr>().unwrap();
    let second_address = "2.3.4.5:80".parse::<SocketAddr>().unwrap();

    let mut message = intproxy.recv().await;

    if matches!(
        message,
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(_))
    ) {
        message = intproxy.recv().await;
    }

    assert_matches!(
        message,
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address }))
        if remote_address == SocketAddress::from(address)
    );

    let local_address = "2.3.4.5:9223".parse::<SocketAddr>().unwrap();

    intproxy
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: address.into(),
                local_address: local_address.into(),
            },
        ))))
        .await;

    // Second
    let mut message = intproxy.recv().await;

    if matches!(
        message,
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(_))
    ) {
        message = intproxy.recv().await;
    }

    assert_matches!(
        message,
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address }))
        if remote_address == SocketAddress::from(second_address)
    );

    intproxy
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 1,
                remote_address: second_address.into(),
                local_address: local_address.into(),
            },
        ))))
        .await;

    test_process.wait().await;
    test_process.assert_stdout_contains("SUCCESS").await;

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
