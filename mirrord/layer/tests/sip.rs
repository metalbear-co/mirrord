#![feature(assert_matches)]
#![cfg(target_os = "macos")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;
use tokio::net::TcpListener;

mod common;

pub use common::*;
use mirrord_sip::sip_patch;

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
    let executable = sip_patch(&executable, &Vec::new()).unwrap().unwrap();
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(dylib_path.to_str().unwrap(), &addr, vec![], None);
    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    // Accept the connection from the layer and verify initial messages.
    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Application subscribed to port, sending tcp messages.");

    layer_connection.expect_gethostname(1).await;

    assert!(layer_connection.is_ended().await);
    test_process.wait().await;
    assert!(!test_process
        .get_stdout()
        .contains("No such file or directory"));
    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
