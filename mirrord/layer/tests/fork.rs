#![feature(assert_matches)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Test that hooks work in a child process after a program calls `fork` without `execve`.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn fork(dylib_path: &PathBuf) {
    let application = Application::Fork;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    println!("waiting for file request.");
    intproxy
        .expect_file_open_with_whatever_options("/path/to/some/file", 1)
        .await;
    intproxy.expect_file_close(1).await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
