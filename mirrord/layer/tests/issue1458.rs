#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1458](https://github.com/metalbear-co/mirrord/issues/1458) is fixed.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[timeout(Duration::from_secs(60))]
async fn test_issue1054(
    #[values(Application::RustIssue1458)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
            ],
            None,
        )
        .await;

    println!("Application subscribed to port, preparing to resolve DNS.");

    test_process.wait_assert_success().await;
    test_process.assert_stdout_contains("test issue 1458: START");
    test_process.assert_stdout_contains("test issue 1458: SUCCESS");
}
