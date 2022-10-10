use std::{path::PathBuf, process::Stdio, time::Duration};

use rstest::rstest;
use tokio::{net::TcpListener, process::Command};

mod common;

pub use common::*;

/// Verify that if mirrord application connects to it own listening port it
/// doesn't go through the layer unnecessarily.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(20))]
async fn test_self_connect(dylib_path: &PathBuf) {
    let application = Application::PythonSelfConnect;
    let executable = application.get_executable().await; // Own it.
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(dylib_path.to_str().unwrap(), &addr);
    let server = Command::new(executable)
        .args(application.get_args())
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    println!("Started application.");

    // Accept the connection from the layer and verify initial messages.
    let mut layer_connection =
        LayerConnection::get_initialized_connection(&listener, application.get_app_port()).await;
    println!("Application subscribed to port, sending tcp messages.");
    assert!(layer_connection.is_ended().await);
    let output = server.wait_with_output().await.unwrap();
    assert!(output.status.success());
    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(output.stderr.is_empty());
    assert!(!&stdout_str.to_lowercase().contains("error"));
}
