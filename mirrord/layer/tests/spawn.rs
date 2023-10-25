#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{collections::HashSet, path::PathBuf, time::Duration};

#[cfg(not(target_os = "macos"))]
use mirrord_protocol::file::XstatResponse;
use mirrord_protocol::{
    file::{OpenFileRequest, OpenFileResponse, ReadFileRequest},
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Some versions of node use `posix_spawn` and not `execve`, so make sure we load into processes
/// that are created by node, with the node version installed where this test is executed.
///
/// The app starts the process `["/bin/sh", "-c", "cat /app/test.txt"]`.
///
/// Since the new process started by the app is bash, it is SIP on mac, so if we don't hook it's
/// spawning we won't load to it. So if we get file requests for `/app/test.txt`, it means we
/// successfully hooked the spawning and patched bash (or we're not on macOS).
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn node_spawn(dylib_path: &PathBuf) {
    let application = Application::NodeSpawn;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_FILE_MODE", "read")], None)
        .await;

    let mut opened_paths = HashSet::new();
    let mut next_remote_fd = 0;

    let mut read_files = HashSet::new();

    while let Some(msg) = intproxy.try_recv().await {
        let ClientMessage::FileRequest(msg) = msg else {
            panic!("unexpected message: {msg:?}");
        };

        match msg {
            FileRequest::Open(OpenFileRequest { path, .. }) => {
                opened_paths.insert(path.to_str().unwrap().to_string());
                intproxy
                    .send(DaemonMessage::File(FileResponse::Open(Ok(
                        OpenFileResponse { fd: next_remote_fd },
                    ))))
                    .await;
                next_remote_fd += 1;
            }
            FileRequest::Read(ReadFileRequest { remote_fd, .. }) => {
                let first_read = read_files.insert(remote_fd);
                let content = if first_read {
                    b"metalbear-hostname".to_vec()
                } else {
                    b"".to_vec()
                };

                intproxy.answer_file_read(content).await;
            }
            #[cfg(not(target_os = "macos"))]
            FileRequest::Xstat(..) => {
                intproxy
                    .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                        XstatResponse {
                            metadata: Default::default(),
                        },
                    ))))
                    .await
            }
            FileRequest::Close(..) => {}
            other => panic!("unexpected message: {other:?}"),
        }
    }

    assert!(
        opened_paths.contains("/app/test.txt"),
        "opened files: {opened_paths:?}"
    );

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
