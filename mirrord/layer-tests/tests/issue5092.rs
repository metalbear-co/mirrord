#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#5092](https://github.com/metalbear-co/mirrord/issues/5092) is fixed: a
/// hooked Go syscall returning a value wider than 32 bits (here, an `mmap` address) reaches the Go
/// caller intact.
#[rstest]
#[tokio::test]
async fn test_issue5092(
    #[values(GoVersion::GO_1_25, GoVersion::GO_1_26, GoVersion::GO_1_27)] go_version: GoVersion,
) {
    let (mut test_process, _intproxy) = Application::GoIssue5092(go_version)
        .start_process(vec![], None)
        .await;
    test_process.wait_assert_success().await;
}
