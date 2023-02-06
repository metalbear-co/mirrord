#![feature(assert_matches)]

use std::{
    assert_matches::assert_matches, collections::HashMap, env::temp_dir, path::PathBuf,
    process::Stdio, time::Duration,
};

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
    println!("{stdout_str}");
    assert!(output.status.success());
    let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(!&stdout_str.to_lowercase().contains("error"));
    assert!(!&stderr_str.to_lowercase().contains("error"));
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
                XstatResponse { metadata },
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
                XstatResponse { metadata },
            ))))
            .await
            .unwrap();
    }
    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();

    // Assert that fwrite flushed correclty
    let data = std::fs::read("/tmp/test_file2.txt").unwrap();
    assert_eq!(
        "Hello, I am the file you're writing!\0",
        &String::from_utf8_lossy(&data)
    );
}

/// Verifies `pwrite` - if opening a file in write mode and writing to it at an offset of zero
/// matches the expected bytes written.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn test_node_close(
    #[values(Application::NodeFileOps)] application: Application,
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
                read: true,
                write: false,
                append: false,
                truncate: false,
                create: false,
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

    let bytes = "hello".as_bytes().to_vec();
    let read_amount = bytes.len();
    // on macOS it will send xstat before reading.
    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            layer_connection.codec.next().await.unwrap().unwrap(),
            ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
                path: None,
                fd: Some(1),
                follow_symlink: true
            }))
        );

        let metadata = MetadataInternal {
            device_id: 0,
            size: read_amount as u64,
            user_id: 2,
            blocks: 3,
            ..Default::default()
        };
        layer_connection
            .codec
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await
            .unwrap();
    }

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
            remote_fd: 1,
            buffer_size: 8192
        }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Read(Ok(
            ReadFileResponse {
                bytes,
                read_amount: read_amount as u64,
            },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
            remote_fd: 1,
            buffer_size: 8192
        }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Read(Ok(
            ReadFileResponse {
                bytes: vec![],
                read_amount: 0,
            },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 1 }))
    );

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
#[cfg(target_os = "linux")]
async fn test_go_stat(
    #[values(Application::Go19FileOps, Application::Go20FileOps)] application: Application,
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
        ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
            path: Some("/tmp/test_file.txt".to_string().into()),
            fd: None,
            follow_symlink: true
        }))
    );

    let metadata = MetadataInternal {
        device_id: 0,
        size: 0,
        user_id: 2,
        blocks: 3,
        ..Default::default()
    };
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Xstat(Ok(
            XstatResponse { metadata },
        ))))
        .await
        .unwrap();
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
#[cfg(target_os = "macos")]
async fn test_go_dir(
    #[values(Application::Go19Dir, Application::Go20Dir)] application: Application,
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
    env.insert("MIRRORD_FILE_READ_ONLY_PATTERN", "/tmp/foo");

    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
            path: "/tmp/foo".to_string().into(),
            open_options: OpenOptionsInternal {
                read: true,
                write: false,
                append: false,
                truncate: false,
                create: false,
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
        ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
            path: None,
            fd: Some(1),
            follow_symlink: true
        }))
    );

    let metadata = MetadataInternal {
        device_id: 0,
        size: 0,
        user_id: 2,
        blocks: 3,
        mode: libc::S_IFDIR as u32,
        ..Default::default()
    };

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Xstat(Ok(
            XstatResponse { metadata: metadata },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 1 }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::OpenDir(Ok(
            OpenDirResponse { fd: 2 },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 2 }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::ReadDir(Ok(
            ReadDirResponse {
                direntry: Some(DirEntryInternal {
                    name: "a".to_string(),
                    inode: 1,
                    position: 1,
                    file_type: libc::DT_REG,
                }),
            },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 2 }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::ReadDir(Ok(
            ReadDirResponse {
                direntry: Some(DirEntryInternal {
                    name: "b".to_string(),
                    inode: 2,
                    position: 2,
                    file_type: libc::DT_REG,
                }),
            },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 2 }))
    );

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::ReadDir(Ok(
            ReadDirResponse { direntry: None },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::CloseDir(CloseDirRequest { remote_fd: 2 }))
    );

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 1 }))
    );

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
#[cfg(target_os = "linux")]
async fn test_go_dir_on_linux(
    #[values(Application::Go19Dir, Application::Go20Dir)] application: Application,
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
    env.insert("MIRRORD_FILE_READ_ONLY_PATTERN", "/tmp/foo");

    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
            path: "/tmp/foo".to_string().into(),
            open_options: OpenOptionsInternal {
                read: true,
                write: false,
                append: false,
                truncate: false,
                create: false,
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

    // Go calls a bare syscall, the layer hooks it and sends the request to the agent.
    assert_matches!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::GetDEnts64(GetDEnts64Request {
            remote_fd: 1,
            .. // Don't want to commit to a specific buffer size.
        }))
    );

    // Simulating the agent: create the response the test expects - two files, "a" and "b".
    let entries = vec![
        DirEntryInternal {
            inode: 1,
            position: 1,
            name: "a".to_string(),
            file_type: libc::DT_REG,
        },
        DirEntryInternal {
            inode: 2,
            position: 2,
            name: "b".to_string(),
            file_type: libc::DT_REG,
        },
    ];

    // The total size of the linux_dirent64 structs in memory.
    let result_size = entries
        .iter()
        .map(|entry| entry.get_d_reclen64())
        .sum::<u16>() as u64;

    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::GetDEnts64(Ok(
            GetDEnts64Response {
                fd: 1,
                entries,
                result_size,
            },
        ))))
        .await
        .unwrap();

    // The caller keeps calling the syscall until it gets an "empty" result.
    assert_matches!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::GetDEnts64(GetDEnts64Request {
            remote_fd: 1,
            ..
        }))
    );

    // "Empty" result: no entries, total size of 0.
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::GetDEnts64(Ok(
            GetDEnts64Response {
                fd: 1,
                entries: vec![],
                result_size: 0,
            },
        ))))
        .await
        .unwrap();

    assert_eq!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 1 }))
    );

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

/// Test that the bypass works for reading dirs with Go.
/// Run with mirrord a go program that opens a dir and fails it does not found expected files in it,
/// then assert it did not fail.
/// Have FS on, but the specific path of the dir local, so that we cover that case where the syscall
/// is hooked, but we bypass.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn test_go_dir_bypass(
    #[values(Application::Go19DirBypass, Application::Go20DirBypass)] application: Application,
    dylib_path: &PathBuf,
) {
    let tmp_dir = temp_dir().join("go_dir_bypass_test");
    std::fs::create_dir_all(tmp_dir.clone()).unwrap();
    std::fs::write(tmp_dir.join("a"), "").unwrap();
    std::fs::write(tmp_dir.join("b"), "").unwrap();

    let executable = application.get_executable().await;
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let mut env = get_env(dylib_path.to_str().unwrap(), &addr);

    let path_string = tmp_dir.to_str().unwrap().to_string();

    // Have FS on so that getdents64 gets hooked.
    env.insert("MIRRORD_FILE_MODE", "read");

    // But make this path local so that in the getdents64 detour we get to the bypass.
    env.insert("MIRRORD_FILE_LOCAL_PATTERN", &path_string);

    let mut test_process =
        TestProcess::start_process(executable, vec![path_string.clone()], env).await;

    let mut _layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

/// Test go file read.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn test_read_go(
    #[values(Application::Go18Read, Application::Go19Read, Application::Go20Read)]
    application: Application,
    dylib_path: &PathBuf,
) {
    let executable = application.get_executable().await;
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let mut env = get_env(dylib_path.to_str().unwrap(), &addr);

    env.insert("MIRRORD_FILE_MODE", "read");

    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");

    let fd = 1;
    layer_connection
        .expect_file_open_for_reading("/app/test.txt", fd)
        .await;

    // Different go versions (mac/linux, 1.18/1.19/1.20) use different amounts of xstat calls here.
    // We accept and answer however many xstat calls the app does, then we verify and answer the
    // read calls.
    layer_connection
        .consume_xstats_then_expect_file_read("Pineapples.", fd)
        .await;

    layer_connection.expect_file_close(fd).await;

    assert!(layer_connection.is_ended().await);

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

/// Test go file read.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn test_write_go(
    #[values(Application::Go18Write, Application::Go19Write, Application::Go20Write)]
    application: Application,
    dylib_path: &PathBuf,
) {
    let executable = application.get_executable().await;
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let mut env = get_env(dylib_path.to_str().unwrap(), &addr);

    env.insert("MIRRORD_FILE_MODE", "localwithoverrides");
    env.insert("MIRRORD_FILE_READ_WRITE_PATTERN", "/app/test.txt");

    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");

    let fd = 1;
    layer_connection
        .expect_file_open_for_writing("/app/test.txt", fd)
        .await;

    layer_connection.expect_xstat(None, Some(fd)).await;

    layer_connection
        .consume_xstats_then_expect_file_write("Pineapples.", 1)
        .await;

    layer_connection.expect_file_close(fd).await;

    assert!(layer_connection.is_ended().await);

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}

/// Test go file read.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn test_lseek_go(
    #[values(Application::Go18Write, Application::Go19Write, Application::Go20LSeek)]
    application: Application,
    dylib_path: &PathBuf,
) {
    let executable = application.get_executable().await;
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let mut env = get_env(dylib_path.to_str().unwrap(), &addr);

    env.insert("MIRRORD_FILE_MODE", "localwithoverrides");
    env.insert("MIRRORD_FILE_READ_WRITE_PATTERN", "/app/test.txt");

    let mut test_process =
        TestProcess::start_process(executable, application.get_args(), env).await;

    let mut layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("Got connection from layer.");

    let fd = 1;
    layer_connection
        .expect_file_open_for_reading("/app/test.txt", fd)
        .await;

    layer_connection.expect_xstat(None, Some(fd)).await;

    layer_connection
        .consume_xstats_then_expect_file_lseek(SeekFromInternal::Current(4), fd)
        .await;

    layer_connection
        .expect_single_file_read("apples.", fd)
        .await;

    // TODO: why don't we get a close request?!
    layer_connection.expect_file_close(fd).await;

    assert!(layer_connection.is_ended().await);

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
}
