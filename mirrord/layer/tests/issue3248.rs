#![cfg(target_os = "macos")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::ClientMessage;
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn skip_sip(dylib_path: &Path, config_dir: &Path) {
    let application = Application::RustIssue3248;
    let config_path = config_dir.join("skip_sip.json");
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], Some(&config_path))
        .await;

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
    test_process
        .assert_stderr_doesnt_contain("DYLD_PRINT_ENV")
        .await;
}
