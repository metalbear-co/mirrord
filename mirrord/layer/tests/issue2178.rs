#![feature(assert_matches)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2178(
    #[values(Application::CIssue2178)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    println!("waiting for file request.");

    intproxy.answer_file_open().await;
    intproxy.answer_file_read(vec![1; 32]).await;
    intproxy.expect_file_close(1).await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 2178: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 2178: SUCESS")
        .await;
}
