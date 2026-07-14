#![cfg(windows)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

#[allow(dead_code)]
mod common;

use common::*;

/// Verifies that a minimal Node process reaches user code after Windows layer initialization.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn empty_node_app_windows(config_dir: &Path) {
    let _tracing = init_tracing();
    let config_path = config_dir.join("windows_layer_integration.toml");
    let application = Application::NodeEmpty;
    let (mut test_process, _intproxy) = application
        .start_process(
            vec![
                (
                    "RUST_LOG",
                    "mirrord=trace,mirrord_layer_lib=trace,mirrord_layer_win=trace,mirrord_intproxy=trace",
                ),
                ("RUST_BACKTRACE", "full"),
            ],
            Some(&config_path),
        )
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_stdout_contains("TEST - SUCCESS").await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
