#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use futures::{stream::StreamExt, SinkExt};
use mirrord_protocol::{file::*, *};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1776](https://github.com/metalbear-co/mirrord/issues/1776) is fixed.
///
/// We test this with `outgoing.udp = false`, as we're just trying to resolve DNS, and not full UDP
/// outgoing traffic.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1899(
    #[values(Application::RustIssue1899)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let fd = 1;

    layer_connection
        .expect_file_open_with_options(
            "/tmp/test_file.txt",
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

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1776: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1776: SUCCESS")
        .await;
}
