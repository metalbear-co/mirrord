#![cfg(target_family = "unix")]
#![cfg(target_os = "macos")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::ClientMessage;
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that mirrord ignores the temp dir with the SIP-patched binaries.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn tmp_dir_read_locally(dylib_path: &Path) {
    let (mut test_process, mut intproxy) =
        Application::BashShebang.start_process(vec![], None).await;

    intproxy
        .expect_file_open_for_reading("/etc/hostname", 1)
        .await;

    intproxy.expect_single_file_read("foobar\n", 1).await;

    match intproxy.recv().await {
        ClientMessage::FileRequest(mirrord_protocol::FileRequest::Close(
            mirrord_protocol::file::CloseFileRequest { fd },
        )) => {
            assert_eq!(fd, 1);
        }
        other => {
            panic!("unexpected message: {other:?}")
        }
    }

    assert_eq!(intproxy.try_recv().await, None);
    test_process.wait().await;
    assert!(
        !test_process
            .get_stdout()
            .await
            .contains("No such file or directory")
    );
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
