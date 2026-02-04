#![feature(assert_matches)]
#![allow(clippy::indexing_slicing)]
#![cfg(not(target_os = "macos"))]
use std::{assert_matches::assert_matches, mem::replace, ops::Not, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    tcp::{DaemonTcp, LayerTcp},
};
use rstest::rstest;

mod common;
pub use common::*;

/// Test that checks the functionality of the closefrom hook, ensuring
/// it doesn't close intproxy connection.
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
            intproxy.recv().await,
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
            intproxy.recv().await,
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)) if
            (40002u16..=40003).contains(&port) &&
                replace(&mut closed[(port - 40000) as usize], true).not()
        );
    }

    // Once the child closes, the first two should close as well.
    for _ in 0..2 {
        assert_matches!(
            intproxy.recv().await,
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)) if
            (40000u16..=40001).contains(&port) &&
                replace(&mut closed[(port - 40000) as usize], true).not()
        );
    }

    assert!(closed.iter().all(|f| *f));

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
