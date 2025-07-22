#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
#![allow(non_snake_case)]

use std::{path::Path, time::Duration};

use nix::{
    sys::{signal, signal::Signal},
    unistd::Pid,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#864](https://github.com/metalbear-co/mirrord/issues/864) is fixed.
///
/// Share sockets between `execve` and `execv` with python's uvicorn.
///
/// We run the `shared_sockets.py` app with the `--reload` flag to trigger the issue.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue864(
    #[values(Application::PythonIssue864)] application: Application,
    dylib_path: &Path,
    config_dir: &Path,
) {
    let (test_process, mut intproxy) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![
                ("RUST_LOG", "mirrord=trace"),
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
                ("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES"),
            ],
            Some(&config_dir.join("port_mapping_shared_sockets.json")),
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

    tokio::time::sleep(Duration::from_secs(10)).await;

    signal::kill(
        Pid::from_raw(test_process.child.id().expect("Child must have pid!") as i32),
        Signal::SIGTERM,
    )
    .expect("Process has been `SIGTERM`!");

    test_process
        .assert_stdout_contains("GET: Request completed")
        .await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
