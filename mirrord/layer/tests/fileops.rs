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
            .into()
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
