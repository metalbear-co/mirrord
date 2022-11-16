#![cfg(target_os = "macos")]
use std::{path::PathBuf, time::Duration};

use mirrord_sip::sip_patch;
use rstest::rstest;
use tokio::net::TcpListener;

mod common;

pub use common::*;

/// Start a web server injected with the layer, simulate the agent, verify expected messages from
/// the layer, send tcp messages and verify in the server output that the application received them.
/// Tests the layer's communication with the agent, the bind hook, and the forwarding of mirrored
/// traffic to the application.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn test_sip_with_bash_script(dylib_path: &PathBuf) {
    let application = Application::EnvBashCat;
    let executable = application.get_executable().await; // Own it.
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(dylib_path.to_str().unwrap(), &addr);
    let patched_executable = sip_patch(&executable).unwrap().unwrap();
    let test_process =
        TestProcess::start_process(patched_executable, application.get_args(), env).await;

    // Accept the connection from the layer and verify initial messages.
    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;

    let fd: usize = 1;

    layer_connection
        .expect_file_open("/very_interesting_file", fd)
        .await;

    layer_connection
        .expect_and_answer_file_read("Very interesting contents.", fd)
        .await;

    layer_connection.expect_file_close(fd).await;

    assert!(layer_connection.is_ended().await);

    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
