use std::{collections::HashMap, path::PathBuf, process::Stdio, time::Duration};

use actix_codec::Framed;
use futures::{stream::StreamExt, SinkExt};
use mirrord_protocol::{tcp::LayerTcp, ClientMessage, DaemonCodec, DaemonMessage};
use rstest::{fixture, rstest};
use tokio::{
    net::{TcpListener, TcpStream},
    process::Command,
};

struct LayerConnection {
    codec: Framed<TcpStream, DaemonCodec>,
}

impl LayerConnection {
    /// Accept a connection from the libraries and verify the first message it is supposed to send
    /// to the agent - GetEnvVarsRequest. Send back a response.
    /// Return the codec of the accepted stream.
    async fn accept_library_connection(listener: &TcpListener) -> Framed<TcpStream, DaemonCodec> {
        let (stream, _) = listener.accept().await.unwrap();
        println!("Got connection from library.");
        let mut codec = Framed::new(stream, DaemonCodec::new());
        let msg = codec.next().await.unwrap().unwrap();
        println!("Got first message from library.");
        if let ClientMessage::GetEnvVarsRequest(request) = msg {
            assert!(request.env_vars_filter.is_empty());
            assert_eq!(request.env_vars_select.len(), 1);
            assert!(request.env_vars_select.contains("*"));
        } else {
            panic!("unexpected request {:?}", msg)
        }
        codec
            .send(DaemonMessage::GetEnvVarsResponse(Ok(HashMap::new())))
            .await
            .unwrap();
        codec
    }

    /// Accept the library's connection and verify initial ENV message and PortSubscribe message
    /// caused by the listen hook.
    async fn get_initialized_connection(listener: &TcpListener) -> LayerConnection {
        let codec = Self::accept_library_connection(listener).await;
        LayerConnection { codec }
    }

    async fn is_ended(&mut self) -> bool {
        self.codec.next().await.is_none()
    }
}

/// Return the path to the existing layer lib, or build it first and return the path, according to
/// whether the environment variable MIRRORD_TEST_USE_EXISTING_LIB is set.
/// When testing locally the lib should be rebuilt on each run so that when developers make changes
/// they don't have to also manually build the lib before running the tests.
/// Building is slow on the CI though, so the CI can set the env var and use an artifact of an
/// earlier job on the same run (there are no code changes in between).
#[fixture]
#[once]
fn dylib_path() -> PathBuf {
    match std::env::var("MIRRORD_TEST_USE_EXISTING_LIB") {
        Ok(path) => {
            let dylib_path = PathBuf::from(path);
            println!("Using existing layer lib from: {:?}", dylib_path);
            assert!(dylib_path.exists());
            dylib_path
        }
        Err(_) => {
            let dylib_path = test_cdylib::build_current_project();
            println!("Built library at {:?}", dylib_path);
            dylib_path
        }
    }
}

/// Verify that mirrord doesn't open remote file if it's the same binary it's running.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(20))]
async fn test_self_open(dylib_path: &PathBuf) {
    let mut env = HashMap::new();
    env.insert("RUST_LOG", "warn,mirrord=debug");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    env.insert("MIRRORD_IMPERSONATED_TARGET", "mock-target"); // Just pass some value.
    env.insert("MIRRORD_CONNECT_TCP", &addr);
    env.insert("MIRRORD_REMOTE_DNS", "false");
    env.insert("DYLD_INSERT_LIBRARIES", dylib_path.to_str().unwrap());
    env.insert("LD_PRELOAD", dylib_path.to_str().unwrap());
    let mut app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    app_path.push("tests/apps/self_open/self_open");
    let server = Command::new(app_path)
        .envs(env)
        .current_dir("/tmp") // if it's the same as the binary it will ignore it by that.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    println!("Started application.");

    // Accept the connection from the layer and verify initial messages.
    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    assert!(layer_connection.is_ended().await);
    let output = server.wait_with_output().await.unwrap();
    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    println!("{}", stdout_str);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!&stdout_str.to_lowercase().contains("error"));
}
