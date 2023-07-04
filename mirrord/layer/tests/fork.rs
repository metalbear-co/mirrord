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
    let (mut test_process, listener) = application
        .get_test_process_and_listener(dylib_path, Default::default(), None)
        .await;
    let _connection_from_parent_process =
        LayerConnection::get_initialized_connection(&listener).await;
    let mut connection_from_child_process =
        LayerConnection::get_initialized_connection(&listener).await;

    println!("waiting for file request.");
    connection_from_child_process
        .expect_file_open_with_whatever_options("/path/to/some/file", 1)
        .await;

    println!("waiting for child process to end.");

    assert!(connection_from_child_process.is_ended().await);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
