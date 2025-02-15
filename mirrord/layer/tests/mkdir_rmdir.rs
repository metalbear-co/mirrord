#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Test for the [`libc::mkdir`], [`libc::mkdirat`] and [`libc::rmdir`] hooks.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn mkdir_rmdir(dylib_path: &Path, config_dir: &Path) {
    let _tracing = init_tracing();
    let application = Application::MkdirRmdir;
    let mut config_path = config_dir.to_path_buf();
    config_path.push("fs.json");
    let config_path = Some(config_path.to_str().unwrap());

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), config_path)
        .await;

    println!("waiting for MakeDirRequest.");
    intproxy
        .expect_make_dir("/mkdir_rmdir_test_dir", 0o777)
        .await;

    println!("waiting for RemoveDirRequest.");
    intproxy.expect_remove_dir("/mkdir_rmdir_test_dir").await;

    println!("waiting for MakeDirRequest.");
    intproxy
        .expect_make_dir("/mkdirat_rmdir_test_dir", 0o777)
        .await;

    println!("waiting for RemoveDirRequest.");
    intproxy.expect_remove_dir("/mkdirat_rmdir_test_dir").await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
