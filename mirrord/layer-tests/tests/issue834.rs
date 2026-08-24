#![cfg(target_family = "unix")]
#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#834](https://github.com/metalbear-co/mirrord/issues/834) is fixed
#[rstest]
#[tokio::test]
async fn test_issue834(
    #[values(GoVersion::GO_1_25, GoVersion::GO_1_26, GoVersion::GO_1_27)] go_version: GoVersion,
) {
    let (mut test_process, _intproxy) = Application::GoIssue834(go_version)
        .start_process(vec![], None)
        .await;

    test_process.wait().await;
    test_process.assert_stdout_contains("okay").await;

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
