#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Tests that sockets are only dropped when all fds pointing to them
/// are closed, not just one.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn dup(dylib_path: &Path) {
    let application = Application::Dup;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer_and_port(dylib_path, Default::default(), None)
        .await;

    // First connection is served, the listener socket is closed.
    intproxy
        .send_connection_then_data("GET / HTTP/1.0\r\n", application.get_app_port())
        .await;

    // Process will close one of the fds now. Make sure we don't get
    // PortUnsubscribe

    match tokio::time::timeout(Duration::from_secs(5), intproxy.recv()).await {
        Ok(message) => {
            panic!("Received {message:?}, not supposed to receive anything");
        }
        Err(_) => {
            // Correct path
        }
    };

    intproxy
        .send_connection_then_data("GET / HTTP/1.0\r\n", application.get_app_port())
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
    test_process
        .assert_stdout_contains("Request 1 served on original socket\n")
        .await;
    test_process
        .assert_stdout_contains("Request 2 served on duplicated socket\n")
        .await;
}
