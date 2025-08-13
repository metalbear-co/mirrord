#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#3456](https://github.com/metalbear-co/mirrord/issues/3456) properly hooks `rename`.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue3456(
    #[values(Application::NodeIssue3456)] application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp")],
            None,
        )
        .await;

    intproxy
        .expect_file_rename("/tmp/krakus_i.pol", "/tmp/krakus_ii.pol")
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 3456: SUCCESS")
        .await;
}
