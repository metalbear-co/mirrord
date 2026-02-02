#![feature(assert_matches)]
use std::{
    assert_matches::{self, assert_matches},
    ops::Not,
    path::Path,
    time::Duration,
};

use mirrord_intproxy_protocol::PortSubscribe;
use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest,
    file::CloseFileRequest,
    tcp::{DaemonTcp, LayerTcp},
};
use rstest::rstest;

mod common;
pub use common::*;

/// Test that hooks work in a child process after a program calls `fork` without `execve`.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn closefrom(dylib_path: &Path) {
    let application = Application::Closefrom;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    for port in 40000..=40003 {
        assert_matches!(
            intproxy.try_recv().await.unwrap(),
            ClientMessage::Tcp(LayerTcp::PortSubscribe(p)) if p == port
        );
        intproxy
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
            .await;
    }

    // Expect the last 2 to be closed first, as the parent process
    // still has a reference to the first two.
    let mut closed = [false; 4];
    for _ in 0..2 {
        assert_matches!(
            intproxy.try_recv().await.unwrap(),
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)) if
            (40002..=40003).includes(port) && {
                closed[port - 40000] = true;
            }
        );
        intproxy
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
            .await;
    }

    while let Some(msg) = intproxy.try_recv().await {
        dbg!(msg);
    }

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
