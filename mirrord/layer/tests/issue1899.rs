#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use mirrord_protocol::{file::*, *};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1899](https://github.com/metalbear-co/mirrord/issues/1899) is fixed.
///
/// Test the `opendir` hook by calling it and asserting that `*DIR` is not `null`.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1899(
    #[values(Application::RustIssue1899)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp")],
            None,
        )
        .await;

    let fd = 1;

    intproxy
        .expect_file_open_with_options(
            "/tmp",
            fd,
            OpenOptionsInternal {
                read: true,
                write: false,
                append: false,
                truncate: false,
                create: false,
                create_new: false,
            },
        )
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 1 }))
    );

    intproxy
        .send(DaemonMessageV1::File(FileResponse::OpenDir(Ok(
            OpenDirResponse { fd: 2 },
        ))))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1899: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1899: SUCCESS")
        .await;
}
