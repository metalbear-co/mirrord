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
async fn mirroring_with_http(
    #[values(
        Application::PythonFlaskHTTP,
        Application::PythonFastApiHTTP,
        Application::NodeHTTP
    )]
    application: Application,
    dylib_path: &PathBuf,
    is_go: bool,
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
            ],
            application
                .get_args()
                .last()
                // flask and go "manually" resolve DNS (they don't use `getaddrinfo`).
                .map(|arg| arg.contains("app_flask.py") || is_go)
                .unwrap_or_default(),
            Some(config_path.to_str().unwrap()),
        )
        .await;

    println!("Application subscribed to port, sending tcp messages.");

    layer_connection
        .send_connection_then_data(
            "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n",
            application.get_app_port(),
        )
        .await;
    layer_connection
        .send_connection_then_data(
            "POST / HTTP/1.1\r\nHost: localhost\r\n\r\npost-data",
            application.get_app_port(),
        )
        .await;
    layer_connection
        .send_connection_then_data(
            "PUT / HTTP/1.1\r\nHost: localhost\r\n\r\nput-data",
            application.get_app_port(),
        )
        .await;
    layer_connection
        .send_connection_then_data(
            "DELETE / HTTP/1.1\r\nHost: localhost\r\n\r\ndelete-data",
            application.get_app_port(),
        )
        .await;

    test_process.wait().await;
    test_process.assert_stdout_contains("GET: Request completed");
    test_process.assert_stdout_contains("POST: Request completed");
    test_process.assert_stdout_contains("PUT: Request completed");
    test_process.assert_stdout_contains("DELETE: Request completed");
    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}

/// Run the http mirroring test only on MacOS, because of a known crash on Linux.
#[cfg(target_os = "macos")]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn mirroring_with_http_go(
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
    #[values(Application::Go19HTTP, Application::Go20HTTP)] application: Application,
) {
    mirroring_with_http(application, dylib_path, is_go(true), config_dir).await;
}
