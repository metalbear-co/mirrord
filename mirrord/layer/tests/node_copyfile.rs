#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{assert_matches::assert_matches, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse, ToPayload,
    file::{
        CloseFileRequest, FchmodRequest, FchownRequest, FtruncateRequest, FutimensRequest,
        MetadataInternal, OpenOptionsInternal, ReadFileResponse, ReadLimitedFileRequest,
        WriteFileRequest, WriteFileResponse,
    },
};
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
#[cfg_attr(target_os = "linux", ignore)]
async fn node_copyfile(dylib_path: &Path) {
    let _tracing = init_tracing();

    let application = Application::NodeCopyFile;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/app/dummy")],
            None,
        )
        .await;

    // Track the file descriptors assigned
    const SRC_FD: u64 = 1;
    const DEST_FD: u64 = 2;

    let payload = "hello".to_payload();
    let payload_len = payload.len();

    intproxy
        .expect_file_open_for_reading("/app/dummy", SRC_FD)
        .await;

    intproxy
        .expect_xstat_with_metadata(
            None,
            Some(SRC_FD),
            MetadataInternal {
                device_id: 0,
                size: payload_len as u64,
                ..Default::default()
            },
        )
        .await;

    intproxy
        .expect_file_open_with_options(
            "/app/dummy.copy",
            DEST_FD,
            OpenOptionsInternal {
                write: true,
                create: true,
                ..Default::default()
            },
        )
        .await;

    intproxy
        .expect_xstat_with_metadata(
            None,
            Some(DEST_FD),
            MetadataInternal {
                device_id: 1,
                ..Default::default()
            },
        )
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Ftruncate(FtruncateRequest { fd: DEST_FD, .. }))
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Ftruncate(Ok(()))))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Futimens(FutimensRequest { fd: DEST_FD, .. }))
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Futimens(Ok(()))))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Fchown(FchownRequest { fd: DEST_FD, .. }))
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Fchown(Ok(()))))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Fchmod(FchmodRequest { fd: DEST_FD, .. }))
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Fchmod(Ok(()))))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::ReadLimited(ReadLimitedFileRequest {
            remote_fd: SRC_FD,
            ..
        }))
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::ReadLimited(Ok(
            ReadFileResponse {
                bytes: payload.clone(),
                read_amount: payload_len as u64,
            },
        ))))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Write(WriteFileRequest { fd: DEST_FD, write_bytes })) if write_bytes == payload
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Write(Ok(
            WriteFileResponse {
                written_amount: payload_len as u64,
            },
        ))))
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest {
            fd: SRC_FD | DEST_FD,
            ..
        }))
    );
    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest {
            fd: DEST_FD | SRC_FD,
            ..
        }))
    );

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
