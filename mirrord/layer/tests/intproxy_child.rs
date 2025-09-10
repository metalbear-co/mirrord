#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Tests that wait(2) calls on the user process won't try to reap the intproxy
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn intproxy_child(dylib_path: &Path) {
    let application = Application::IntproxyChild;
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
