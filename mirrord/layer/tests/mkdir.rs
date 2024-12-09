#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Test for the [`libc::mkdir`] function.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn mkdir(dylib_path: &Path) {
    let application = Application::MakeDir;

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    println!("waiting for file request.");
    intproxy.expect_make_dir("/mkdir_test_path", 0o777).await;

    println!("waiting for file request.");
    intproxy.expect_make_dir("/mkdirat_test_path", 0o777).await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
