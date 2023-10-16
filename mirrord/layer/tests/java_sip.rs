#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;
use tokio::net::TcpListener;

mod common;
pub use common::*;

/// Regression test for https://github.com/metalbear-co/mirrord/issues/1459
/// Test that Java Termulin installed via SDKMan can run with mirrord.
///
/// This test requires there to be a java binary at ~/.sdkman/candidates/java/17.0.6-tem/bin/java to
/// pass.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn java_temurin_sip(dylib_path: &PathBuf) {
    let application = Application::JavaTemurinSip;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");

    let env = get_env_no_fs(dylib_path.to_str().unwrap(), &addr);

    let mut test_process = application.get_test_process(env).await;
    let _intproxy = TestIntProxy::new(listener).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
