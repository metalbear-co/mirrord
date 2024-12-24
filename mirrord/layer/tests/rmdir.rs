#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Test for the [`libc::rmdir`] function.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn rmdir(dylib_path: &Path) {
    let application = Application::RemoveDir;

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    println!("waiting for MakeDirRequest.");
    intproxy.expect_make_dir("/test_dir", 0o777).await;

    println!("waiting for RemoveDirRequest.");
    intproxy.expect_remove_dir("/test_dir").await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
