#![warn(clippy::indexing_slicing)]
#![allow(non_snake_case)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

const METHODS: [&str; 4] = ["GET", "POST", "PUT", "DELETE"];

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
        Application::GoHTTP(GoVersion::GO_1_24),
        Application::GoHTTP(GoVersion::GO_1_25),
        Application::GoHTTP(GoVersion::GO_1_26)
    )]
    application: Application,
    config_dir: &Path,
) {
    let _guard = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_port(
            vec![
                ("MIRRORD_LOG", "mirrord=trace"),
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
            ],
            Some(&config_dir.join("port_mapping.json")),
        )
        .await;

    println!("Application subscribed to port, sending HTTP requests.");

    send_mirrored_requests(&mut intproxy, &application).await;

    // Wait for the application to report each request instead of relying on its exit, so that if
    // the exit hangs below, it's clearly distinguishable from a mirroring failure.
    wait_for_all_requests(&test_process).await;

    // The application exits on its own after serving all requests, which also captures all of its
    // remaining output before the assertions below. The node app has been observed hanging inside
    // `process.exit()` on loaded CI runners after successfully serving everything (CI flake first
    // seen 2026-06-15, previously an opaque rstest timeout). Fail fast on such a hang, dumping
    // per-thread diagnostics needed for root-causing it.
    if tokio::time::timeout(Duration::from_secs(15), test_process.wait())
        .await
        .is_err()
    {
        dump_exit_hang_diagnostics(test_process.child.id());
        panic!("application served all requests but did not exit within grace period");
    }

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}

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

/// Sends one mirrored connection per HTTP method in [`METHODS`] to the application.
async fn send_mirrored_requests(intproxy: &mut TestIntProxy, application: &Application) {
    for method in METHODS {
        let content = if method == "GET" {
            String::new()
        } else {
            format!("{}-data", method.to_lowercase())
        };
        intproxy
            .send_connection_then_data(
                &prepare_request_body(method, &content),
                application.get_app_port(),
            )
            .await;
    }
}

/// Waits until the application reports handling each request from [`METHODS`].
async fn wait_for_all_requests(test_process: &TestProcess) {
    for method in METHODS {
        test_process
            .wait_for_line_stdout(
                Duration::from_secs(40),
                &format!("{method}: Request completed"),
            )
            .await;
    }
}

/// Prints the state and kernel wait channel of every thread of the hung test app, under a
/// greppable `EXIT_HANG` marker.
fn dump_exit_hang_diagnostics(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    println!("EXIT_HANG_DETECTED: test app (pid {pid}) served all requests but did not exit");

    #[cfg(target_os = "linux")]
    {
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            return;
        };
        for task in tasks.flatten() {
            let read = |name: &str| {
                std::fs::read_to_string(task.path().join(name))
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            };
            let state = read("stat")
                .split_whitespace()
                .nth(2)
                .unwrap_or("?")
                .to_owned();
            println!(
                "EXIT_HANG_THREAD: tid={} comm={:?} state={state} wchan={}",
                task.file_name().to_string_lossy(),
                read("comm"),
                read("wchan"),
            );
        }
    }
}
