#![feature(assert_matches)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

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
#[timeout(Duration::from_secs(60))]
async fn node_spawn(dylib_path: &PathBuf) {
    let application = Application::NodeSpawn;
    let (mut test_process, listener) = application
        .get_test_process_and_listener(dylib_path, vec![("MIRRORD_FILE_MODE", "local")], None)
        .await;

    // Accept the connection from the layer and verify initial messages.
    let _node_layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("NODE LAYER CONNECTION HANDLED");

    // Accept the connection from the layer and verify initial messages.
    let mut sh_layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    println!("SH LAYER CONNECTION HANDLED");

    if cfg!(macos) {
        let mut bash_layer_connection =
            LayerConnection::get_initialized_connection(&listener).await;
        println!("BASH LAYER CONNECTION HANDLED");
        bash_layer_connection.handle_gethostname().await;
    } else {
        sh_layer_connection.handle_gethostname().await;
    }

    // TODO(alex) [high] 2023-04-13: Handle the `gethostname` dance for the bash program.

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
