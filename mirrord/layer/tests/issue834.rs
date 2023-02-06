#![feature(assert_matches)]

#[cfg(target_os = "linux")]
use std::{path::PathBuf, time::Duration};

#[cfg(target_os = "linux")]
use rstest::rstest;
#[cfg(target_os = "linux")]
use tokio::net::TcpListener;

#[cfg(target_os = "linux")]
mod common;

#[cfg(target_os = "linux")]
pub use common::*;

/// Verify that issue [#834](https://github.com/metalbear-co/mirrord/issues/834) is fixed
#[cfg(target_os = "linux")]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn test_issue834(
    #[values(
        Application::Go18Issue834,
        Application::Go19Issue834,
        Application::Go20Issue834
    )]
    application: Application,
    dylib_path: &PathBuf,
) {
    let executable = application.get_executable().await; // Own it.
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(dylib_path.to_str().unwrap(), &addr);
    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    // Accept the connection from the layer and verify initial messages.
    let mut _layer_connection = LayerConnection::get_initialized_connection(&listener).await;

    test_process.wait().await;
    test_process.assert_stdout_contains("okay");

    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
