#![cfg(target_os = "linux")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn test_issue2204(
    #[values(Application::RustIssue2204)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    intproxy
        .expect_xstat(Some(PathBuf::from("/some/remote/file/1")), None)
        .await;
    intproxy
        .expect_file_open_for_reading("/some/remote/file/2", 222)
        .await;
    intproxy.expect_xstat(None, Some(222)).await;
    intproxy.expect_file_close(222).await;

    test_process.wait_assert_success().await;
}
