#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1054](https://github.com/metalbear-co/mirrord/issues/1054) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1054(
    #[values(Application::RustIssue1054)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let mut config_path = config_dir.clone();
    config_path.push("port_mapping.json");

    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
                ("MIRRORD_REMOTE_DNS", "false"),
            ],
            Some(config_path.to_str().unwrap()),
        )
        .await;

    println!("Application subscribed to port, sending tcp messages.");

    let new_connection_id = layer_connection
        .send_new_connection(application.get_app_port())
        .await;
    layer_connection.send_close(new_connection_id).await;

    test_process.wait_assert_success().await;
    test_process.assert_stdout_contains("test issue 1054: START");
    test_process.assert_stdout_contains("test issue 1054: SUCCESS");
}
