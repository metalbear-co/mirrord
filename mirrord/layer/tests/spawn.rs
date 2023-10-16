#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use mirrord_protocol::{ClientMessage, FileRequest};
use rstest::rstest;

mod common;

pub use common::*;

/// Some versions of node use `posix_spawn` and not `execve`, so make sure we load into processes
/// that are created by node, with the node version installed where this test is executed.
///
/// Since the new process started by the app is bash, it is SIP on mac, so if we don't hook it's
/// spawning we won't load to it. So if we get a second layer connection, it means we successfully
/// hooked the spawning and patched bash (or we're not on macOS).
/// The app starts the process `["/bin/sh", "-c", "echo \"Hello over shell\""]`.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn node_spawn(dylib_path: &PathBuf) {
    let application = Application::NodeSpawn;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_FILE_MODE", "local")], None)
        .await;

    if cfg!(target_os = "macos") {
        // flow isn't deterministic, so just handle everything until it ends.
        loop {
            let msg = intproxy.try_recv().await;
            match msg {
                Some(ClientMessage::FileRequest(FileRequest::Open(_))) => {
                    intproxy.answer_file_open().await
                }
                Some(ClientMessage::FileRequest(FileRequest::Read(_))) => {
                    intproxy
                        .answer_file_read(b"metalbear-hostname".to_vec())
                        .await
                }
                Some(ClientMessage::FileRequest(FileRequest::Close(_))) => {}
                None => break,
                _ => {
                    panic!("Unexpected message from layer connection {msg:?}")
                }
            }
        }
    } else {
        intproxy.handle_gethostname::<true>(None).await;
    };

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
