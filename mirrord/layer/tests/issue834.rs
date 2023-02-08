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
    let (mut test_process, _layer_connection) = application
        .start_process_with_layer(dylib_path, vec![])
        .await;

    test_process.wait().await;
    test_process.assert_stdout_contains("okay");

    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
