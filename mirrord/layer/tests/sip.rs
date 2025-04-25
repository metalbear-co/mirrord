#![feature(assert_matches)]
#![cfg(target_os = "macos")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::ClientMessage;
use rstest::rstest;
use tokio::net::TcpListener;

mod common;

pub use common::*;
use mirrord_sip::{sip_patch, SipPatchOptions};

/// Verify that mirrord ignores the temp dir with the SIP-patched binaries.
/// If it does not, it would try to read the script from the remote pod.
/// We assert `is_ended` right after the initial messages, making sure the layer does not try to
/// read the file remotely.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn tmp_dir_read_locally(dylib_path: &Path) {
    let application = Application::BashShebang;
    let executable = application.get_executable().await;
    let executable = sip_patch(&executable, SipPatchOptions::default())
        .unwrap()
        .unwrap();
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(dylib_path, addr, vec![], None);
    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    // Accept the connection from the layer and verify initial messages.
    let mut intproxy = TestIntProxy::new(listener).await;
    println!("Application subscribed to port, sending tcp messages.");

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
    assert!(!test_process
        .get_stdout()
        .await
        .contains("No such file or directory"));
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
