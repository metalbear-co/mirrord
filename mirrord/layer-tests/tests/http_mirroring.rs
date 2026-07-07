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
                ("RUST_LOG", "mirrord=trace"),
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
            ],
            Some(&config_dir.join("port_mapping.json")),
        )
        .await;

    println!("Application subscribed to port, sending HTTP requests.");

    send_mirrored_requests(&mut intproxy, &application).await;

    // Wait for the application to report each request instead of relying on its exit, so an exit
    // problem (covered by `app_exits_after_mirrored_requests`) can't mask the result of what this
    // test actually verifies.
    wait_for_all_requests(&test_process).await;

    // The application exits on its own after serving all requests. Wait for it, so that on the
    // normal path it exits gracefully and all of its output is captured before the assertions
    // below. Don't fail on a hang though - exit behavior is owned by
    // `app_exits_after_mirrored_requests` - just fall back to the kill on `test_process` drop.
    if tokio::time::timeout(Duration::from_secs(10), test_process.wait())
        .await
        .is_err()
    {
        println!("application did not exit in time, proceeding with assertions");
    }

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Verify the application manages to exit on its own after serving mirrored traffic.
///
/// The node application has been observed hanging inside `process.exit()` under the layer on
/// loaded CI runners after successfully serving all requests (CI flake first seen 2026-06-15,
/// previously manifesting as a `mirroring_with_http` timeout). This test isolates that exit
/// behavior from the traffic-mirroring assertions, repeats the scenario to raise the reproduction
/// odds, and dumps thread diagnostics when the hang reproduces.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(180))]
async fn app_exits_after_mirrored_requests(
    #[values(Application::NodeHTTP)] application: Application,
    config_dir: &Path,
) {
    let _guard = init_tracing();

    for attempt in 0..3 {
        println!("attempt {attempt}: starting application");

        let (mut test_process, mut intproxy) = application
            .start_process_with_port(
                vec![
                    ("RUST_LOG", "mirrord=trace"),
                    ("MIRRORD_FILE_MODE", "local"),
                    ("MIRRORD_UDP_OUTGOING", "false"),
                ],
                Some(&config_dir.join("port_mapping.json")),
            )
            .await;

        send_mirrored_requests(&mut intproxy, &application).await;
        wait_for_all_requests(&test_process).await;

        // The application exits itself after serving all requests.
        if tokio::time::timeout(Duration::from_secs(30), test_process.child.wait())
            .await
            .is_err()
        {
            dump_exit_hang_diagnostics(test_process.child.id());
            panic!(
                "application served all requests but did not exit within grace period \
                (attempt {attempt})"
            );
        }
    }
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
