#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::time::Duration;

use rstest::rstest;

mod common;
pub use common::*;

/// Regression test for <https://github.com/metalbear-co/mirrord/issues/1459>
/// Test that Java Temurin installed via SDKMan can run with mirrord.
///
/// This test requires there to be a java binary at ~/.sdkman/candidates/java/17.0.6-tem/bin/java to
/// pass, or set `MIRRORD_TEST_JAVA_PATH` to override.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn java_temurin_sip() {
    let (mut test_process, _intproxy) = Application::JavaTemurinSip
        .start_process(vec![("MIRRORD_FILE_MODE", "local")], None)
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
