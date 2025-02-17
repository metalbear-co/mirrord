#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Test for the [`libc::statfs`] and [`libc::fstatfs`] functions.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn statfs(dylib_path: &Path) {
    let application = Application::StatfsFstatfs;

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    println!("waiting for file request (statfs).");
    intproxy.expect_statfs("/statfs_fstatfs_test_path").await;

    println!("waiting for file request (open).");
    let fd: u64 = 1;
    intproxy
        .expect_file_open_for_reading("/statfs_fstatfs_test_path", fd)
        .await;

    println!("waiting for file request (fstatfs).");
    intproxy.expect_fstatfs(fd).await;

    println!("waiting for file request (close).");
    intproxy.expect_file_close(fd).await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
