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
use tokio::net::TcpListener;

mod common;

pub use common::*;

/// Verify that issue [#864](https://github.com/metalbear-co/mirrord/issues/864) is fixed.
///
/// Share sockets between `execve` and `execv` with python's uvicorn.
///
/// We run the `shared_sockets.py` app with the `--reload` flag to trigger the issue.
/// The worker completion line verifies request handling. A successful parent exit checks
/// parent shutdown, but does not establish the worker's exit status.
#[rstest]
#[tokio::test]
async fn test_issue864(
    #[values(Application::PythonIssue864)] application: Application,
    config_dir: &Path,
) {
    let config = config_dir.join("port_mapping_shared_sockets.toml");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let env = get_env(
        listener.local_addr().unwrap(),
        vec![
            ("MIRRORD_LOG", "mirrord=info"),
            ("MIRRORD_FILE_MODE", "local"),
            ("MIRRORD_UDP_OUTGOING", "false"),
        ],
    );
    let mut test_process = application.get_test_process(env, Some(&config)).await;
    let pid = Pid::from_raw(test_process.child.id().unwrap() as i32);
    let mut intproxy = None;

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

    // A startup or request failure must still let the parent stop and join its worker.
    let operation = AssertUnwindSafe(async {
        let proxy = intproxy.insert(
            tokio::time::timeout(
                Duration::from_secs(30),
                TestIntProxy::new_with_app_port(
                    listener,
                    application.get_app_port(),
                    Some(&config),
                ),
            )
            .await
            .expect("uvicorn did not subscribe to its port within 30 seconds"),
        );
        println!("Application subscribed to port, sending HTTP requests.");
        tokio::time::timeout(Duration::from_secs(60), async {
            proxy
                .send_connection_then_data(
                    &prepare_request_body("GET", ""),
                    application.get_app_port(),
                )
                .await;
            test_process
                .wait_for_line_stdout(Duration::from_secs(20), "GET: Request completed")
                .await;
        })
        .await
        .expect("uvicorn did not print GET: Request completed within 60 seconds");
    })
    .catch_unwind()
    .await;

    let signal_result = signal::kill(pid, Signal::SIGTERM);
    let exit = AssertUnwindSafe(async {
        tokio::time::timeout(Duration::from_secs(15), test_process.wait_assert_success())
            .await
            .expect(
                "uvicorn did not exit and finish reading output within 15 seconds after SIGTERM",
            );
    })
    .catch_unwind()
    .await;

    if let Err(error) = &signal_result {
        eprintln!("failed to send SIGTERM to uvicorn: {error}");
    }
    if operation.is_err() && exit.is_err() {
        eprintln!("uvicorn exit check also failed; returning the earlier failure");
    }
    if let Err(payload) = operation {
        resume_unwind(payload);
    }
    if let Err(payload) = exit {
        resume_unwind(payload);
    }
    signal_result.expect("failed to send SIGTERM to uvicorn");
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
