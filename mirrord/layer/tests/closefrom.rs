#![feature(assert_matches)]
use std::{
    assert_matches::{self, assert_matches},
    path::Path,
    time::Duration,
};

use mirrord_intproxy_protocol::PortSubscribe;
use mirrord_protocol::{
    ClientMessage, DaemonMessage,
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

    intproxy
        .expect_file_open_with_whatever_options("/a/file", 4)
        .await;
    intproxy
        .expect_file_open_with_whatever_options("/some/other/file", 5)
        .await;
    intproxy
        .expect_file_open_with_whatever_options("/yet/another_file", 6)
        .await;
    intproxy
        .expect_file_open_with_whatever_options("/take/a/wild/guess", 7)
        .await;
    intproxy
        .expect_file_open_with_whatever_options("/oh/wow", 8)
        .await;

    assert_matches!(
        intproxy.try_recv().await.unwrap(),
        ClientMessage::Tcp(LayerTcp::PortSubscribe(12345))
    );

    intproxy
        .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(12345))))
        .await;

    while let Some(msg) = intproxy.try_recv().await {
        dbg!(msg);
    }

    intproxy.expect_file_close(1).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
