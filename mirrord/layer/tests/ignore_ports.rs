#![cfg(target_os = "linux")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Start an application (and load the layer into it) that listens on a port that is configured to
/// be ignored, and verify that no messages are sent to the agent.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn ignore_ports(
    #[values(Application::PythonListen)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let mut config_path = config_dir.clone();
    config_path.push("ignore_ports.json");
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_MODE", "local")],
            Some(config_path.to_str().unwrap()),
        )
        .await;

    // Make sure no listen request was made.
    assert!(layer_connection.is_ended().await);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}
