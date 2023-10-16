#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use mirrord_protocol::{file::*, *};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2001](https://github.com/metalbear-co/mirrord/issues/2001) is fixed.
///
/// Test the `readdir` (linux and macos) and `readdir64` (linux) hooks.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2001(
    #[values(Application::RustIssue2001)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp")],
            None,
        )
        .await;

    intproxy
        .expect_file_open_with_options(
            "/tmp",
            10,
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
        ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 10 }))
    );

    intproxy
        .send(DaemonMessage::File(FileResponse::OpenDir(Ok(
            OpenDirResponse { fd: 11 },
        ))))
        .await;

    intproxy.expect_file_close(10).await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 11 })),
    );

    intproxy
        .send(DaemonMessage::File(FileResponse::ReadDir(Ok(
            ReadDirResponse {
                direntry: Some(DirEntryInternal {
                    inode: 1,
                    position: 0,
                    name: "file1".into(),
                    file_type: libc::DT_REG,
                }),
            },
        ))))
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 11 })),
    );

    intproxy
        .send(DaemonMessage::File(FileResponse::ReadDir(Ok(
            ReadDirResponse {
                direntry: Some(DirEntryInternal {
                    inode: 2,
                    position: 1,
                    name: "file2".into(),
                    file_type: libc::DT_REG,
                }),
            },
        ))))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 2001: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 2001: SUCCESS")
        .await;
}
