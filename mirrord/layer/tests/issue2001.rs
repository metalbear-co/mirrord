#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use futures::{stream::StreamExt, SinkExt};
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
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp")],
            None,
        )
        .await;

    let fd = 1;

    layer_connection
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
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 1 }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::OpenDir(Ok(
            OpenDirResponse { fd: 2 },
        ))))
        .await
        .unwrap();

    layer_connection.expect_file_close(1).await;

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 2 })),
    );

    layer_connection
        .codec
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
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 2 })),
    );

    layer_connection
        .codec
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
        .await
        .unwrap();

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 2001: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 2001: SUCCESS")
        .await;
}
