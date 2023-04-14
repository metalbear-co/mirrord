#![feature(assert_matches)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// For running locally, so that new developers don't have the extra step of building the go app
/// before running the tests.
#[cfg(target_os = "macos")]
#[ctor::ctor]
fn build_go_app() {
    use std::{env, path::Path, process};
    let original_dir = env::current_dir().unwrap();
    let go_app_path = Path::new("tests/apps/app_go");
    env::set_current_dir(go_app_path).unwrap();
    let output = process::Command::new("go")
        .args(vec!["build", "-o", "19"])
        .output()
        .expect("Failed to build Go test app.");
    assert!(output.status.success(), "Building Go test app failed.");
    env::set_current_dir(original_dir).unwrap();
}

/// Start a web server injected with the layer, simulate the agent, verify expected messages from
/// the layer, send tcp messages and verify in the server output that the application received them.
/// Tests the layer's communication with the agent, the bind hook, and the forwarding of mirrored
/// traffic to the application.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    // Make sure listen request was made.
    assert!(layer_connection.is_ended().await);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}
