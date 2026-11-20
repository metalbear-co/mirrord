#![cfg(target_family = "unix")]
#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

mod common;

pub use common::*;

/// Verify that issue [#834](https://github.com/metalbear-co/mirrord/issues/834) is fixed
#[tokio::test]
async fn test_issue834() {
    // `//mirrord/layer-tests/apps/issue834` builds the app with one Go version only.
    let (mut test_process, _intproxy) = Application::GoIssue834(GoVersion::GO_1_25)
        .start_process(vec![], None)
        .await;

    test_process.wait().await;
    test_process.assert_stdout_contains("okay").await;

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
