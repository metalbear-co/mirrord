#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

use core::assert_matches;
use std::path::{Path, PathBuf};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
    file::{MetadataInternal, XstatRequest, XstatResponse},
};
use rstest::rstest;

mod common;

pub use common::*;

/// libuv stats paths and fds with `statx` through libc's `syscall`, so the layer only sees
/// `statSync`, `lstatSync` and `fstatSync` with `experimental.hook_libc_syscall`.
#[rstest]
#[tokio::test]
async fn node_stat(config_dir: &Path) {
    let _tracing = init_tracing();

    const FD: u64 = 1;
    let file = MetadataInternal {
        mode: 0o100644,
        size: 5,
        ..Default::default()
    };

    let config = config_dir.join("node_libuv.json");
    let (mut test_process, mut intproxy) = Application::NodeStat
        .start_process(
            vec![
                ("MIRRORD_FILE_MODE", "localwithoverrides"),
                ("MIRRORD_FILE_READ_ONLY_PATTERN", "^/app/"),
            ],
            Some(&config),
        )
        .await;

    intproxy
        .expect_xstat_with_metadata(Some(PathBuf::from("/app/file")), None, file)
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
            path: Some(path),
            fd: None,
            follow_symlink: false,
        })) if path == Path::new("/app/link")
    );
    intproxy
        .send(DaemonMessage::File(FileResponse::Xstat(Ok(
            XstatResponse {
                metadata: MetadataInternal {
                    mode: 0o120777,
                    ..Default::default()
                },
            },
        ))))
        .await;

    intproxy.expect_file_open_for_reading("/app/file", FD).await;
    intproxy
        .expect_xstat_with_metadata(None, Some(FD), file)
        .await;
    intproxy.expect_file_close(FD).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}
