#![cfg(target_family = "unix")]

use std::{io, path::PathBuf};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse, ResponseError, file::XstatRequest,
};
use rstest::rstest;

mod common;
pub use common::*;

/// Test for the [`libc::realpath`] function.
///
/// The layer makes a remote path absolute by itself, and only asks the agent whether the result
/// exists, with an [`XstatRequest`]. The app's local paths must not reach the agent.
#[rstest]
#[tokio::test]
async fn realpath() {
    let (mut test_process, mut intproxy) = Application::Realpath
        .start_process(Default::default(), None)
        .await;

    // `absolute_remotely` and `absolute_remotely_malloc` both resolve the same existing path.
    for _ in 0..2 {
        intproxy
            .expect_xstat(Some(PathBuf::from("/app/data.txt")), None)
            .await;
    }

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
            path: Some(PathBuf::from("/app/missing.txt")),
            fd: None,
            follow_symlink: true,
        }))
    );
    // The agent answers for a missing file with the error of its own `stat`.
    intproxy
        .send(DaemonMessage::File(FileResponse::Xstat(Err(
            ResponseError::from(io::Error::from_raw_os_error(libc::ENOENT)),
        ))))
        .await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
