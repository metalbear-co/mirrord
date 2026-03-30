#![cfg(windows)]
#![warn(clippy::indexing_slicing)]

use std::time::Duration;

use rstest::rstest;

#[allow(dead_code)]
mod common;

use common::*;

/// Validate that Python's socketpair works under mirrord on Windows.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn python_socketpair_windows() {
    let application = Application::PythonSocketPair;
    let (mut test_process, mut intproxy) = application.start_process(vec![], None).await;

    // Ensure no unexpected mirrord messages are sent for local socketpair.
    match tokio::time::timeout(Duration::from_secs(2), intproxy.try_recv()).await {
        Ok(Some(msg)) => panic!("unexpected message from intproxy: {msg:?}"),
        Ok(None) | Err(_) => {}
    }

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
