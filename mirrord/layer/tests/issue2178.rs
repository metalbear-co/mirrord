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

    intproxy
        .expect_file_open_with_read_flag("/app/test.txt", 3)
        .await;
    assert_eq!(intproxy.expect_only_file_read(3).await, 12);
    let file_data = "abcdefgh".as_bytes().to_vec();
    intproxy.answer_file_read(file_data).await;
    intproxy.expect_file_close(3).await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 2178: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 2178: SUCCESS")
        .await;
}
