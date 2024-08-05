#![feature(assert_matches)]
#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#834](https://github.com/metalbear-co/mirrord/issues/834) is fixed
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue834(
    #[values(
        Application::Go19Issue834,
        Application::Go20Issue834,
        Application::Go21Issue834
    )]
    application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    test_process.wait().await;
    test_process.assert_stdout_contains("okay").await;

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
