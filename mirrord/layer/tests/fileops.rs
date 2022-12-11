use std::{collections::HashMap, path::PathBuf, process::Stdio, time::Duration};

use actix_codec::Framed;
use futures::{stream::StreamExt, SinkExt};
use mirrord_protocol::{file::*, *};
use rstest::rstest;
use tokio::{
    net::{TcpListener, TcpStream},
    process::Command,
};
mod common;
pub use common::*;

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
    env.insert("MIRRORD_IMPERSONATED_TARGET", "pod/mock-target"); // Just pass some value.
    env.insert("MIRRORD_CONNECT_TCP", &addr);
    env.insert("MIRRORD_REMOTE_DNS", "false");
    env.insert("DYLD_INSERT_LIBRARIES", dylib_path.to_str().unwrap());
    env.insert("LD_PRELOAD", dylib_path.to_str().unwrap());
    let mut app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    app_path.push("tests/apps/self_open/19");
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

/// Verifies `pwrite` - if opening a file in write mode and writing to it at an offset of zero
/// matches the expected bytes written.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn test_pwrite(
    #[values(Application::RustFileOps)] application: Application,
    dylib_path: &PathBuf,
) {
    let executable = application.get_executable().await;
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let mut env = get_env(dylib_path.to_str().unwrap(), &addr);

    env.insert("MIRRORD_FILE_MODE", "read");
    // add rw override for the specific path
    env.insert("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/test_file.txt");

    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");
    // pwrite test
    // reply to open
    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
            path: "/tmp/test_file.txt".to_string().into(),
            open_options: OpenOptionsInternal {
                read: false,
                write: true,
                append: false,
                truncate: false,
                create: true,
                create_new: false,
            },
        }))
    );
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Open(Ok(
            OpenFileResponse { fd: 1 },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::WriteLimited(WriteLimitedFileRequest {
            remote_fd: 1,
            start_from: 0,
            write_bytes: vec![
                72, 101, 108, 108, 111, 44, 32, 73, 32, 97, 109, 32, 116, 104, 101, 32, 102, 105,
                108, 101, 32, 121, 111, 117, 39, 114, 101, 32, 119, 114, 105, 116, 105, 110, 103,
                33, 0
            ]
        }))
    );

    // reply to pwrite
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::WriteLimited(Ok(
            WriteFileResponse { written_amount: 37 },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 1 }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Close(Ok(
            CloseFileResponse {},
        ))))
        .await
        .unwrap();
    // Rust compiles with newer libc on Linux that uses statx
    #[cfg(target_os = "macos")]
    {
        // lstat test
        assert_eq!(
            layer_connection.codec.next().await.unwrap().unwrap(),
            ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
                path: Some("/tmp/test_file.txt".to_string().into()),
                fd: None,
                follow_symlink: false
            }))
        );

        let metadata = MetadataInternal {
            device_id: 0,
            size: 1,
            user_id: 2,
            blocks: 3,
            ..Default::default()
        };
        layer_connection
            .codec
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata: metadata },
            ))))
            .await
            .unwrap();

        // fstat test
        assert_eq!(
            layer_connection.codec.next().await.unwrap().unwrap(),
            ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
                path: Some("/tmp/test_file.txt".to_string().into()),
                fd: None,
                follow_symlink: true
            }))
        );

        let metadata = MetadataInternal {
            device_id: 4,
            size: 5,
            user_id: 6,
            blocks: 7,
            ..Default::default()
        };
        layer_connection
            .codec
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata: metadata },
            ))))
            .await
            .unwrap();
    }
    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_stderr_empty();

    // Assert that fwrite flushed correclty
    let data = std::fs::read("/tmp/test_file2.txt").unwrap();
    assert_eq!(
        "Hello, I am the file you're writing!\0",
        &String::from_utf8_lossy(&data)
    );
}
