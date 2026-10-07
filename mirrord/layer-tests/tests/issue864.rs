#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]
#![allow(non_snake_case)]

use std::{
    panic::{AssertUnwindSafe, resume_unwind},
    path::Path,
    time::Duration,
};

use futures::FutureExt;
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
async fn test_issue864(
    #[values(Application::PythonIssue864)] application: Application,
    config_dir: &Path,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_port(
            vec![
                ("MIRRORD_LOG", "mirrord=info"),
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
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

    let request = AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(60),
        test_process.wait_for_line_stdout(Duration::from_secs(20), "GET: Request completed"),
    ))
    .catch_unwind()
    .await;

    let signal_result = match test_process.child.id() {
        Some(pid) => signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
            .map_err(|error| format!("SIGTERM failed: {error}")),
        None => Err("reload parent has no PID for SIGTERM".to_owned()),
    };

    // A failed signal must not skip the sole wait/drain attempt.
    let cleanup = AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(15),
        test_process.wait(),
    ))
    .catch_unwind()
    .await;

    eprintln!("reload-parent signal outcome: {signal_result:?}");
    match &cleanup {
        Ok(Ok(status)) => {
            eprintln!("parent wait and both-reader drain completed: {status}");
        }
        Ok(Err(error)) => {
            eprintln!("INCOMPLETE cleanup: 15-second wait/drain timeout: {error}");
        }
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("non-string cleanup panic payload");
            eprintln!("INCOMPLETE cleanup: wait/drain panic: {text}");
        }
    }

    // Report signal/cleanup evidence before restoring the original request failure.
    match request {
        Err(payload) => resume_unwind(payload),
        Ok(Err(error)) => {
            panic!("request completion exceeded 60 seconds: {error}; cleanup outcome above");
        }
        Ok(Ok(())) => {}
    }

    let status = match cleanup {
        Err(payload) => resume_unwind(payload),
        Ok(Err(error)) => panic!("parent wait/drain exceeded 15 seconds: {error}"),
        Ok(Ok(status)) => status,
    };
    if let Err(error) = signal_result {
        panic!("{error}; parent wait/drain outcome above");
    }
    assert!(
        status.success(),
        "reload parent exited unsuccessfully: {status}"
    );

    test_process
        .assert_stdout_contains("GET: Request completed")
        .await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
