#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1123](https://github.com/metalbear-co/mirrord/issues/1123) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1123(
    #[values(Application::RustIssue1123)] application: Application,
    dylib_path: &PathBuf,
) {
    // `_layer_connection` has to be kept alive, otherwise we crash before the app is done.
    let (mut test_process, _layer_connection) = application
        .start_process_with_layer(
            dylib_path,
            vec![
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
                ("MIRRORD_REMOTE_DNS", "false"),
            ],
            None,
        )
        .await;

    println!("Application subscribed to port, sending tcp messages.");

    test_process.wait_assert_success().await;
    test_process.assert_stdout_contains("test issue 1123: START");
    test_process.assert_stdout_contains("test issue 1123: SUCCESS");
}
