#![feature(assert_matches)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that if mirrord application connects to it own listening port it
/// doesn't go through the layer unnecessarily.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn self_connect(dylib_path: &PathBuf) {
    let application = Application::PythonSelfConnect;
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer_and_port(dylib_path, vec![("MIRRORD_FILE_MODE", "local")], false)
        .await;
    assert!(layer_connection.is_ended().await);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}
