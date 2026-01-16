#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
#![allow(non_snake_case)]

use std::{path::Path, time::Duration};

use rstest::rstest;

use serde_json::json;

use mirrord_tests::utils::ManagedTempFile;

mod common;

pub use common::*;

/// Start an HTTP server injected with the layer, simulate the agent, verify expected messages from
/// the layer, send HTTP requests and verify in the server output that the application received
/// them. Tests the layer's communication with the agent, the bind hook, and the forwarding of
/// mirrored traffic to the application.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn mirroring_with_http(
    #[values(
        Application::PythonFlaskHTTP,
        Application::PythonFastApiHTTP,
        Application::NodeHTTP,
        Application::GoHTTP(GoVersion::GO_1_23),
        Application::GoHTTP(GoVersion::GO_1_24),
        Application::GoHTTP(GoVersion::GO_1_25)
    )]
    application: Application,
    dylib_path: &Path,
    config_dir: &Path,
) {
    let _guard = init_tracing();

    let config_json = json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "mirror",
                    "port_mapping": [[9999, 1234]]
                }
            }
        }
    });
    let tempfile = ManagedTempFile::new(config_json);
    let config_path = config_dir.join(&tempfile.path); 

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![
                ("RUST_LOG", "mirrord=trace"),
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
                ("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES"),
            ],
            Some(&config_path),
        )
        .await;

    println!("Application subscribed to port, sending HTTP requests.");

    fn prepare_request_body(method: &str, content: &str) -> String {
        let content_headers = if content.is_empty() {
            String::new()
        } else {
            format!(
                "content-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n",
                content.len()
            )
        };

        format!("{method} / HTTP/1.1\r\nhost: localhost\r\n{content_headers}\r\n{content}",)
    }

    intproxy
        .send_connection_then_data(&prepare_request_body("GET", ""), application.get_app_port())
        .await;
    intproxy
        .send_connection_then_data(
            &prepare_request_body("POST", "post-data"),
            application.get_app_port(),
        )
        .await;
    intproxy
        .send_connection_then_data(
            &prepare_request_body("PUT", "put-data"),
            application.get_app_port(),
        )
        .await;
    intproxy
        .send_connection_then_data(
            &prepare_request_body("DELETE", "delete-data"),
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
