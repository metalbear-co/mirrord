#![feature(assert_matches)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Start a web server injected with the layer, simulate the agent, verify expected messages from
/// the layer, send tcp messages and verify in the server output that the application received them.
/// Tests the layer's communication with the agent, the bind hook, and the forwarding of mirrored
/// traffic to the application.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
#[cfg(target_os = "linux")]
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
