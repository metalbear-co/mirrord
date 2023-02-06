#![feature(assert_matches)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;
use tokio::net::TcpListener;

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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(20))]
async fn test_node_spawn(dylib_path: &PathBuf) {
    let application = Application::NodeSpawn;
    let executable = application.get_executable().await;
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env_no_fs(dylib_path.to_str().unwrap(), &addr);
    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    // Accept the connection from the layer and verify initial messages.
    let _node_layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    // Accept the connection from the layer and verify initial messages.
    let _shell_layer_connection = LayerConnection::get_initialized_connection(&listener).await;

    test_process.wait().await;
    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
