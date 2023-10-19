#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Some versions of node use `posix_spawn` and not `execve`, so make sure we load into processes
/// that are created by node, with the node version installed where this test is executed.
///
/// The app starts the process `["/bin/sh", "-c", "cat /app/test.txt"]`.
///
/// Since the new process started by the app is bash, it is SIP on mac, so if we don't hook it's
/// spawning we won't load to it. So if we get file requests for `/app/test.txt`, it means we
/// successfully hooked the spawning and patched bash (or we're not on macOS).
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn node_spawn(dylib_path: &PathBuf) {
    let application = Application::NodeSpawn;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_FILE_MODE", "read")], None)
        .await;

    intproxy
        .expect_file_open_for_reading("/app/test.txt", 2)
        .await;

    #[cfg(not(target_os = "macos"))]
    {
        intproxy.expect_xstat(None, Some(2)).await;
    }

    intproxy
        .expect_file_read("Very interesting contents.", 2)
        .await;

    intproxy.expect_file_close(2).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
