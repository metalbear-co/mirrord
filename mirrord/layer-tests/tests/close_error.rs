#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

//! Checks that an error the agent reports for a close reaches the application.

use std::{io, path::PathBuf};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse, ResponseError, file::CloseFileRequest,
};
use rstest::rstest;

mod common;

pub use common::*;

/// The application (`apps/close_error.py`) opens a remote file and closes it twice. The agent
/// fails the first close with `ENOSPC`, as a filesystem that writes data out on close does.
///
/// The first `close` returns that error, and the layer forgets the fd anyway, like the kernel
/// does: the second `close` fails locally with `EBADF` and sends nothing to the agent.
#[rstest]
#[tokio::test]
async fn close_reports_remote_error() {
    let app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/apps/close_error.py");
    let application = Application::DynamicApp(
        Application::get_python3_executable().await,
        vec!["-u".to_owned(), app_path.to_string_lossy().into_owned()],
    );

    let (mut test_process, mut intproxy) = application
        .start_process(
            vec![
                ("MIRRORD_FILE_MODE", "localwithoverrides"),
                ("MIRRORD_FILE_READ_WRITE_PATTERN", "^/mirrord-test/"),
            ],
            None,
        )
        .await;

    let fd = 1;
    intproxy
        .expect_file_open_for_reading("/mirrord-test/close-error", fd)
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd })),
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Close(Err(
            ResponseError::from(io::Error::from_raw_os_error(libc::ENOSPC)),
        ))))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("first close failed: ENOSPC")
        .await;
    test_process
        .assert_stdout_contains("second close failed: EBADF")
        .await;
    assert_eq!(intproxy.try_recv().await, None);
}
