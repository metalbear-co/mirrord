#![cfg(target_family = "unix")]
#![feature(assert_matches)]

use std::{path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    tcp::{DaemonTcp, LayerTcp},
};
use rstest::rstest;

mod common;
pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn dup_listen(dylib_path: &Path) {
    let application = Application::DupListen;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::Tcp(LayerTcp::PortSubscribe(80)),
    );
    intproxy
        .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(80))))
        .await;

    // Wait until the test app does dup+close.
    let log = test_process
        .await_exactly_n_lines(1, Duration::from_secs(5))
        .await;
    assert_eq!(log.first().unwrap(), "Duplicated descriptor closed",);
    // It should not trigger port unsubscribe.
    tokio::time::timeout(Duration::from_secs(5), intproxy.recv())
        .await
        .unwrap_err();

    // Verify that the subscription works fine - test app waits for this data before exiting.
    intproxy.send_connection_then_data("hello there", 80).await;
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
