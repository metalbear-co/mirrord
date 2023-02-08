use std::{path::PathBuf, time::Duration};

use rstest::rstest;
use tokio::net::TcpListener;

mod common;

pub use common::*;

/// Verify that if mirrord application connects to it own listening port it
/// doesn't go through the layer unnecessarily.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn test_self_connect(dylib_path: &PathBuf) {
    let application = Application::PythonSelfConnect;
    let executable = application.get_executable().await; // Own it.
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env_no_fs(dylib_path.to_str().unwrap(), &addr);
    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    // Accept the connection from the layer and verify initial messages.
    let mut layer_connection = LayerConnection::get_initialized_connection_with_port(
        &listener,
        application.get_app_port(),
    )
    .await;
    println!("Application subscribed to port, sending tcp messages.");
    assert!(layer_connection.is_ended().await);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}
