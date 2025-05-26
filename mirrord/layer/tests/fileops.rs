#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

#[cfg(target_os = "linux")]
use std::assert_matches::assert_matches;
#[cfg(target_os = "macos")]
use std::{env, fs};
use std::{
    env::temp_dir,
    path::{Path, PathBuf},
    time::Duration,
};

use libc::{pid_t, O_RDWR};
use mirrord_protocol::{file::*, *};
#[cfg(target_os = "macos")]
use mirrord_sip::{sip_patch, SipPatchOptions, MIRRORD_TEMP_BIN_DIR_PATH_BUF};
use nix::{
    sys::signal::{self, Signal},
    unistd::Pid,
};
use rstest::rstest;

mod common;
pub use common::*;

fn get_rw_test_file_env_vars() -> Vec<(&'static str, &'static str)> {
    vec![
        ("MIRRORD_FILE_MODE", "localwithoverrides"),
        ("MIRRORD_FILE_READ_WRITE_PATTERN", "/app/test.txt"),
    ]
}

/// Verify that mirrord doesn't open remote file if it's the same binary it's running.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn self_open(
    #[values(
        Application::Go21SelfOpen,
        Application::Go22SelfOpen,
        Application::Go23SelfOpen
    )]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    assert_eq!(intproxy.try_recv().await, None);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

/// Verify that if the user's app is trying to read out of mirrord's temp bin dir for some messed up
/// reason (actually shouldn't happen, this is a second line of defence), that we hook that and the
/// file is read from the path outside of that dir,
/// e.g.: app tries to read /tmp/mirrord-bin/usr/local/foo, then make it read from /usr/local/foo.
#[cfg(target_os = "macos")]
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn read_from_mirrord_bin(dylib_path: &Path) {
    let _tracing = init_tracing();

    let contents = "please don't flake";
    let temp_dir = env::temp_dir();
    let file_path = temp_dir.join("mirrord-test-read-from-mirrord-bin");

    // write contents to <TMPDIR>/mirrord-test-read-from-mirrord-bin.
    fs::write(&file_path, contents).unwrap();

    // <TMPDIR>/mirrord-bin/<TMPDIR>/mirrord-test-read-from-mirrord-bin.
    let path_in_mirrord_bin =
        MIRRORD_TEMP_BIN_DIR_PATH_BUF.join(file_path.strip_prefix("/").unwrap());

    // Make sure we write and read from different paths (this is "meta check").
    assert_ne!(file_path, path_in_mirrord_bin);

    let executable = sip_patch("cat", SipPatchOptions::default())
        .unwrap()
        .unwrap();

    // <TMPDIR>/mirrord-bin/cat <TMPDIR>/mirrord-bin/<TMPDIR>/mirrord-test-read-from-mirrord-bin
    let application = Application::DynamicApp(
        executable,
        vec![path_in_mirrord_bin.to_string_lossy().to_string()],
    );

    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;

    // We read the contents from <TMPDIR>/<OUR-FILE> even though the app tried to read from
    // <TMPDIR>/mirrord-bin/<TMPDIR>/<OUR-FILE>.
    test_process.assert_stdout_contains(contents).await;
}

/// Verifies `pwrite` - if opening a file in write mode and writing to it at an offset of zero
/// matches the expected bytes written.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn pwrite(#[values(Application::RustFileOps)] application: Application, dylib_path: &Path) {
    let _tracing = init_tracing();

    // add rw override for the specific path
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/test_file.txt")],
            None,
        )
        .await;

    let fd = 1;

    intproxy
        .expect_file_open_with_options(
            "/tmp/test_file.txt",
            fd,
            OpenOptionsInternal {
                read: false,
                write: true,
                append: false,
                truncate: false,
                create: true,
                create_new: false,
            },
        )
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::WriteLimited(WriteLimitedFileRequest {
            remote_fd: fd,
            start_from: 0,
            write_bytes: vec![
                72, 101, 108, 108, 111, 44, 32, 73, 32, 97, 109, 32, 116, 104, 101, 32, 102, 105,
                108, 101, 32, 121, 111, 117, 39, 114, 101, 32, 119, 114, 105, 116, 105, 110, 103,
                33, 0
            ]
        }))
    );

    // reply to pwrite
    intproxy
        .send(DaemonMessage::File(FileResponse::WriteLimited(Ok(
            WriteFileResponse { written_amount: 37 },
        ))))
        .await;

    intproxy.expect_file_close(fd).await;

    // Rust compiles with newer libc on Linux that uses statx
    #[cfg(target_os = "macos")]
    {
        // lstat test
        assert_eq!(
            intproxy.recv().await,
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
        intproxy
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await;

        // fstat test
        assert_eq!(
            intproxy.recv().await,
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
        intproxy
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await;
    }
    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;

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
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn node_close(
    #[values(Application::NodeFileOps)] application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            // add rw override for the specific path
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/test_file.txt")],
            None,
        )
        .await;

    let fd = 1;

    intproxy
        .expect_file_open_for_reading("/tmp/test_file.txt", fd)
        .await;

    let contents = "hello";
    // on macOS it will send xstat before reading.
    #[cfg(target_os = "macos")]
    {
        let read_amount = contents.len();
        assert_eq!(
            intproxy.recv().await,
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
        intproxy
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await;
    }

    intproxy.expect_file_read(contents, fd).await;

    intproxy.expect_file_close(fd).await;

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test `chdir` functionality with remote operations:
/// 1. App creates a directory remotely (e.g., /tmp/remote_dir).
/// 2. App changes CWD to that remote directory.
/// 3. App creates a file using a relative path (e.g., "relative_file.txt").
/// 4. Layer should resolve this relative path to /tmp/remote_dir/relative_file.txt for the agent.
/// 5. App writes to the file.
/// 6. App reads the file (using full or relative path) to verify content.
/// 7. App cleans up the file and directory.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(90))] // Increased timeout for multiple ops and clarity
async fn test_chdir_and_relative_file_creation(
    #[values(Application::RustFileOps)] application: Application, // Assuming RustFileOps can be controlled by ENV
    dylib_path: &Path,
) {
    let _tracing = init_tracing();
    let unique_id = uuid::Uuid::new_v4();
    let base_remote_dir = PathBuf::from(format!("/tmp/mirrord_chdir_test_{}", unique_id));
    let relative_file_name = PathBuf::from("test_file_in_chdir.txt");
    let absolute_remote_file_path = base_remote_dir.join(&relative_file_name);
    let file_content = format!("Content for chdir test {}", unique_id);

    // Environment variables to instruct the test application (RustFileOps)
    let env_vars = vec![
        ("MIRRORD_FILE_MODE", "readwrite"), // General permission
        ("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/.*"), // Allow /tmp operations
        ("TEST_ACTION_CREATE_DIR_ALL", base_remote_dir.to_str().unwrap()),
        ("TEST_ACTION_CHDIR", base_remote_dir.to_str().unwrap()),
        (
            "TEST_ACTION_CREATE_FILE_RELATIVE",
            relative_file_name.to_str().unwrap(),
        ),
        ("TEST_ACTION_WRITE_CONTENT", &file_content),
        (
            "TEST_ACTION_READ_VERIFY_FILE_ABSOLUTE",
            absolute_remote_file_path.to_str().unwrap(),
        ),
        (
            "TEST_ACTION_DELETE_FILE_ABSOLUTE",
            absolute_remote_file_path.to_str().unwrap(),
        ),
        (
            "TEST_ACTION_DELETE_DIR_ABSOLUTE",
            base_remote_dir.to_str().unwrap(),
        ),
    ];

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, env_vars, None)
        .await;

    // --- Step 1: App creates remote directory ---
    // mkdir (or mkdirat if create_dir_all resolves to it)
    // Assuming create_dir_all on /tmp/foo results in a MakeDirRequest for /tmp/foo
    intproxy
        .expect_file_request(FileRequest::MakeDir(MakeDirRequest {
            pathname: base_remote_dir.clone(),
            mode: 0o777, // Rust's create_dir_all default mode for the final component
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::MakeDir(Ok(()))))
        .await;

    // --- Step 2: App changes CWD to that remote directory ---
    intproxy
        .expect_file_request(FileRequest::Chdir(ChdirRequest {
            path: base_remote_dir.clone(),
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::Chdir(Ok(
            ChdirResponse {},
        ))))
        .await;

    // --- Step 3 & 4: App creates and writes to a file with a relative path ---
    // The layer's `ops::open` (for File::create) MUST resolve `relative_file_name`
    // using the new `CURRENT_DIR` to `absolute_remote_file_path`.
    let fd_file_create = 3; // Example FD
    intproxy
        .expect_file_open_with_options(
            &absolute_remote_file_path, // Expecting resolved path
            fd_file_create,
            OpenOptionsInternal {
                read: false,
                write: true,
                append: false,
                truncate: true,
                create: true,
                create_new: false,
            },
        )
        .await;

    intproxy
        .expect_file_write(file_content.as_bytes().to_vec(), fd_file_create)
        .await;
    intproxy.expect_file_close(fd_file_create).await;

    // --- Step 5: App reads the file to verify content (using absolute path) ---
    let fd_file_read = 4; // Example FD
    intproxy
        .expect_file_open_for_reading(&absolute_remote_file_path, fd_file_read)
        .await;
    intproxy
        .consume_xstats_then_expect_file_read(&file_content, fd_file_read)
        .await;
    intproxy.expect_file_close(fd_file_read).await;

    // --- Step 6: App removes the file (using absolute path) ---
    intproxy
        .expect_file_request(FileRequest::Unlink(UnlinkRequest {
            pathname: absolute_remote_file_path.clone(),
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::Unlink(Ok(()))))
        .await;

    // --- Step 7: App removes the directory (using absolute path) ---
    intproxy
        .expect_file_request(FileRequest::RemoveDir(RemoveDirRequest {
            pathname: base_remote_dir.clone(),
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::RemoveDir(Ok(()))))
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test `chdir` functionality:
/// 1. Create a directory remotely.
/// 2. Change CWD to that directory.
/// 3. Create a file with a relative path.
/// 4. Verify the file was created in the new CWD on the remote pod.
/// 5. Clean up.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))] // Increased timeout for multiple ops
async fn test_chdir_remote_ops(
    #[values(Application::RustFileOps)] application: Application, // Using a simple Rust app
    dylib_path: &Path,
) {
    let _tracing = init_tracing();
    let unique_id = uuid::Uuid::new_v4();
    let base_dir_name = format!("/tmp/mirrord_chdir_test_{}", unique_id);
    let file_name = "test_file_in_chdir.txt";
    let remote_dir_path = PathBuf::from(&base_dir_name);
    let remote_file_path = remote_dir_path.join(file_name);
    let file_content = "Hello from chdir test!";

    // Configure mirrord to allow R/W operations in /tmp/
    let env = vec![
        ("MIRRORD_FILE_MODE", "readwrite"),
        ("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/.*"),
    ];

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, env, None)
        .await;

    // 1. Create remote directory
    intproxy
        .expect_file_request(FileRequest::MakeDir(MakeDirRequest {
            pathname: remote_dir_path.clone(),
            mode: 0o777, // Default mode for std::fs::create_dir
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::MakeDir(Ok(()))))
        .await;

    // 2. Change current directory (this will be part of the application's execution)
    // We expect a ChdirRequest. The application binary for RustFileOps would need to be
    // modified or a new one created to actually call std::env::set_current_dir.
    // For now, we'll simulate this by having the test app call it.
    // The `RustFileOps` app calls `set_current_dir` if "CALL_CHDIR" env var is set.
    // We'll assume it's called with `remote_dir_path`.
    // No, the test itself calls the std lib functions.

    // Expect Xstat for path existence check before chdir (often done by libc or app)
    // This might vary depending on libc implementation. Let's be flexible or skip if too flaky.
    // For now, let's assume no xstat before chdir for simplicity in this specific test flow,
    // as std::env::set_current_dir might not trigger it directly in all libc versions.

    intproxy
        .expect_file_request(FileRequest::Chdir(ChdirRequest {
            path: remote_dir_path.clone(),
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::Chdir(Ok(
            ChdirResponse {},
        ))))
        .await;

    // 3. Create file with relative path (after chdir)
    let fd_file_create = 3; // Assign a plausible fd for the new file
    intproxy
        .expect_file_open_with_options(
            &remote_file_path, // Agent should see the full path after chdir
            fd_file_create,
            OpenOptionsInternal {
                read: false,
                write: true,
                append: false,
                truncate: true,
                create: true,
                create_new: false,
            },
        )
        .await;

    // 4. Write to the file
    intproxy
        .expect_file_write(file_content.as_bytes().to_vec(), fd_file_create)
        .await;
    intproxy.expect_file_close(fd_file_create).await;

    // 5. Verify file content by reading it (remotely)
    let fd_file_read = 4;
    intproxy
        .expect_file_open_for_reading(&remote_file_path, fd_file_read)
        .await;
    intproxy
        .consume_xstats_then_expect_file_read(file_content, fd_file_read)
        .await;
    intproxy.expect_file_close(fd_file_read).await;

    // 6. Teardown: Remove file
    intproxy
        .expect_file_request(FileRequest::Unlink(UnlinkRequest {
            pathname: remote_file_path.clone(),
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::Unlink(Ok(()))))
        .await;

    // 7. Teardown: Remove directory
    intproxy
        .expect_file_request(FileRequest::RemoveDir(RemoveDirRequest {
            pathname: remote_dir_path.clone(),
        }))
        .await;
    intproxy
        .send(DaemonMessage::File(FileResponse::RemoveDir(Ok(()))))
        .await;

    // Execute the test binary's main function which should perform these ops
    // The RustFileOps app needs to be modified or a new app created
    // to perform: create_dir, set_current_dir, File::create, write, read_to_string, remove_file, remove_dir
    // For this test, we'll assume the test process directly calls these.
    // The `application.start_process_with_layer` runs the app.
    // We need an app that performs these actions.
    // Let's use a generic Rust app and pass commands or use env vars to guide its actions.
    // Or, more simply, the test itself can perform these actions if the dylib is loaded
    // into the test process itself. However, these tests run the app as a child process.

    // For now, this test structure defines the agent interactions.
    // The actual RustFileOps or a new app would need to perform:
    // std::fs::create_dir_all(&remote_dir_path).unwrap();
    // std::env::set_current_dir(&remote_dir_path).unwrap();
    // let mut file = std::fs::File::create(file_name).unwrap(); // relative path
    // file.write_all(file_content.as_bytes()).unwrap();
    // drop(file); // close
    // let content_read = std::fs::read_to_string(&remote_file_path).unwrap(); // full path for verification
    // assert_eq!(content_read, file_content);
    // std::fs::remove_file(&remote_file_path).unwrap();
    // std::fs::remove_dir(&remote_dir_path).unwrap();

    // The test process itself doesn't have the dylib loaded. The child process does.
    // So, we need `application` to be an app that performs these actions.
    // `Application::RustFileOps` is a generic one. We might need to adapt it or add a new one.
    // Let's assume `Application::RustChdirOps` exists and performs these steps.
    // If not, this test is more of a blueprint for agent interaction.

    // Let's simplify and assume the `application` (e.g. RustFileOps) is already set up
    // to perform these actions based on some arguments or environment variables.
    // The main point is to verify the sequence of hooks and agent interactions.

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
#[cfg(target_os = "linux")]
async fn go_stat(
    #[values(
        Application::Go21FileOps,
        Application::Go22FileOps,
        Application::Go23FileOps
    )]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    // add rw override for the specific path
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp/test_file.txt")],
            None,
        )
        .await;

    let fd = 1;

    intproxy
        .expect_file_open_with_options(
            "/tmp/test_file.txt",
            fd,
            OpenOptionsInternal {
                read: false,
                write: true,
                append: false,
                truncate: false,
                create: true,
                create_new: false,
            },
        )
        .await;

    assert_eq!(
        intproxy.recv().await,
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
    intproxy
        .send(DaemonMessage::File(FileResponse::Xstat(Ok(
            XstatResponse { metadata },
        ))))
        .await;

    intproxy.expect_statfs("/tmp/test_file.txt").await;
    intproxy.expect_fstatfs(fd).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[cfg(target_os = "macos")]
async fn go_dir(
    #[values(Application::Go21Dir, Application::Go22Dir, Application::Go23Dir)]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_ONLY_PATTERN", "/tmp/foo")],
            None,
        )
        .await;

    let fd = 1;
    intproxy.expect_file_open_for_reading("/tmp/foo", fd).await;

    let mut message = intproxy.recv().await;
    if matches!(message, ClientMessage::FileRequest(FileRequest::Xstat(..))) {
        assert_eq!(
            message,
            ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
                path: None,
                fd: Some(fd),
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

        intproxy
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await;

        message = intproxy.recv().await;
    }

    assert_eq!(
        message,
        ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 1 }))
    );

    let dir_fd = 2;
    intproxy
        .send(DaemonMessage::File(FileResponse::OpenDir(Ok(
            OpenDirResponse { fd: dir_fd },
        ))))
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::ReadDirBatch(ReadDirBatchRequest {
            remote_fd: dir_fd,
            amount: 128
        }))
    );

    intproxy
        .send(DaemonMessage::File(FileResponse::ReadDirBatch(Ok(
            ReadDirBatchResponse {
                fd: dir_fd,
                dir_entries: vec![
                    DirEntryInternal {
                        name: "a".to_string(),
                        inode: 1,
                        position: 1,
                        file_type: libc::DT_REG,
                    },
                    DirEntryInternal {
                        name: "b".to_string(),
                        inode: 2,
                        position: 2,
                        file_type: libc::DT_REG,
                    },
                ],
            },
        ))))
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::ReadDirBatch(ReadDirBatchRequest {
            remote_fd: dir_fd,
            amount: 128
        }))
    );

    intproxy
        .send(DaemonMessage::File(FileResponse::ReadDirBatch(Ok(
            ReadDirBatchResponse {
                fd: dir_fd,
                dir_entries: Vec::new(),
            },
        ))))
        .await;

    assert_eq!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::CloseDir(CloseDirRequest { remote_fd: dir_fd }))
    );
    intproxy.expect_file_close(fd).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[cfg(target_os = "linux")]
async fn go_dir_on_linux(
    #[values(Application::Go21Dir, Application::Go22Dir, Application::Go23Dir)]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_READ_ONLY_PATTERN", "/tmp/foo")],
            None,
        )
        .await;

    let fd = 1;
    intproxy.expect_file_open_for_reading("/tmp/foo", fd).await;

    // Go calls a bare syscall, the layer hooks it and sends the request to the agent.
    assert_matches!(
        intproxy.recv().await,
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

    intproxy
        .send(DaemonMessage::File(FileResponse::GetDEnts64(Ok(
            GetDEnts64Response {
                fd: 1,
                entries,
                result_size,
            },
        ))))
        .await;

    // The caller keeps calling the syscall until it gets an "empty" result.
    assert_matches!(
        intproxy.recv().await,
        ClientMessage::FileRequest(FileRequest::GetDEnts64(GetDEnts64Request {
            remote_fd: 1,
            ..
        }))
    );

    // "Empty" result: no entries, total size of 0.
    intproxy
        .send(DaemonMessage::File(FileResponse::GetDEnts64(Ok(
            GetDEnts64Response {
                fd: 1,
                entries: vec![],
                result_size: 0,
            },
        ))))
        .await;

    intproxy.expect_file_close(fd).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test that the bypass works for reading dirs with Go.
/// Run with mirrord a go program that opens a dir and fails it does not found expected files in it,
/// then assert it did not fail.
/// Have FS on, but the specific path of the dir local, so that we cover that case where the syscall
/// is hooked, but we bypass.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn go_dir_bypass(
    #[values(
        Application::Go21DirBypass,
        Application::Go22DirBypass,
        Application::Go23DirBypass
    )]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let tmp_dir = temp_dir().join("go_dir_bypass_test");
    std::fs::create_dir_all(tmp_dir.clone()).unwrap();
    std::fs::write(tmp_dir.join("a"), "").unwrap();
    std::fs::write(tmp_dir.join("b"), "").unwrap();

    let path_string = tmp_dir.to_str().unwrap().to_string();

    // But make this path local so that in the getdents64 detour we get to the bypass.
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![
                ("MIRRORD_TEST_GO_DIR_BYPASS_PATH", &path_string),
                ("MIRRORD_FILE_LOCAL_PATTERN", &path_string),
            ],
            None,
        )
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test go file read and close.
/// This test also verifies the close hook, since go's `os.ReadFile` calls `Close`.
/// We don't call close in other tests because Go does not wait for the operation to complete before
/// returning, which means we can't just normally wait in the test for the message from the layer
/// because the app could close before the message is sent.
/// What we do here in order to avoid race conditions is that the Go test app for this test waits
/// for a signal after calling `Close` (implicitly, by calling `ReadFile`), and the test sends the
/// signal to the app only once the close message was verified. Only then does the test app exit.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn read_go(
    #[values(Application::Go21Read, Application::Go22Read, Application::Go23Read)]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_FILE_MODE", "read")], None)
        .await;

    let fd = 1;
    intproxy
        .expect_file_open_for_reading("/app/test.txt", fd)
        .await;

    // Different go versions (mac/linux, 1.18/1.19/1.20) use different amounts of xstat calls here.
    // We accept and answer however many xstat calls the app does, then we verify and answer the
    // read calls.
    intproxy
        .consume_xstats_then_expect_file_read("Pineapples.", fd)
        .await;

    intproxy.expect_file_close(fd).await;
    // Notify Go test app that the close detour completed and it can exit.
    // (The go app waits for this, since Go does not wait for the close detour to complete before
    // returning from `Close`).
    signal::kill(
        Pid::from_raw(test_process.child.id().unwrap() as pid_t),
        Signal::SIGTERM,
    )
    .unwrap();

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test go file write.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn write_go(
    #[values(Application::Go21Write, Application::Go22Write, Application::Go23Write)]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(dylib_path, get_rw_test_file_env_vars(), None)
        .await;

    let fd = 1;
    layer_connection
        .expect_file_open_for_writing("/app/test.txt", fd)
        .await;

    layer_connection
        .consume_xstats_then_expect_file_write("Pineapples.", 1)
        .await;

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test go file lseek.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn lseek_go(
    #[values(Application::Go21LSeek, Application::Go22LSeek, Application::Go23LSeek)]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, get_rw_test_file_env_vars(), None)
        .await;

    let fd = 1;
    intproxy
        .expect_file_open_with_read_flag("/app/test.txt", fd)
        .await;

    intproxy
        .consume_xstats_then_expect_file_lseek(SeekFromInternal::Current(4), fd)
        .await;

    intproxy.expect_single_file_read("apples.", fd).await;

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}

/// Test go file access.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn faccessat_go(
    #[values(
        Application::Go21FAccessAt,
        Application::Go22FAccessAt,
        Application::Go23FAccessAt
    )]
    application: Application,
    dylib_path: &Path,
) {
    let _tracing = init_tracing();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, get_rw_test_file_env_vars(), None)
        .await;

    intproxy
        .expect_file_access(PathBuf::from("/app/test.txt"), O_RDWR as u8)
        .await;

    // Assert all clear
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
}
