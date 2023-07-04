#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Start an HTTP server injected with the layer, simulate the agent, verify expected messages from
/// the layer, send HTTP requests and verify in the server output that the application received
/// them. Tests the layer's communication with the agent, the bind hook, and the forwarding of
/// mirrored traffic to the application.
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
    config_dir: &PathBuf,
) {
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
            ],
            Some(config_dir.join("port_mapping.json").to_str().unwrap()),
        )
        .await;

    println!("Application subscribed to port, sending HTTP requests.");

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
    test_process
        .assert_stdout_contains("GET: Request completed")
        .await;
    test_process
        .assert_stdout_contains("POST: Request completed")
        .await;
    test_process
        .assert_stdout_contains("PUT: Request completed")
        .await;
    test_process
        .assert_stdout_contains("DELETE: Request completed")
        .await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
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
    mirroring_with_http(application, dylib_path, config_dir).await;
}
