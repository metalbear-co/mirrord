#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use futures::StreamExt;
use mirrord_protocol::{ClientMessage, FileRequest};
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
#[tokio::test]
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

    // There is a 3rd layer connection that happens on macos, where `/bin/sh` starts `bash`, and
    // thus we have to handle the `gethostname` messasges after it.
    let _last_layer_connection = if cfg!(target_os = "macos") {
        let mut bash_layer_connection =
            LayerConnection::get_initialized_connection(&listener).await;
        println!("BASH LAYER CONNECTION HANDLED");
        // flow isn't deterministic, so just handle everything until it ends.
        loop {
            let msg = bash_layer_connection.codec.next().await;
            match msg {
                Some(Ok(ClientMessage::FileRequest(FileRequest::Open(_)))) => {
                    bash_layer_connection.answer_file_open().await
                }
                Some(Ok(ClientMessage::FileRequest(FileRequest::Read(_)))) => {
                    bash_layer_connection
                        .answer_file_read(b"metalbear-hostname".to_vec())
                        .await
                }
                Some(Ok(ClientMessage::FileRequest(FileRequest::Close(_)))) => {}
                None => break,
                _ => {
                    panic!("Unexpected message from bash layer connection {msg:?}")
                }
            }
        }
        bash_layer_connection
    } else {
        // Meanwhile on linux, we handle it after the 2nd connection, in the `/bin/sh` handler.
        sh_layer_connection.handle_gethostname::<true>(None).await;
        sh_layer_connection
    };

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stdout();
    test_process.assert_no_error_in_stderr();
}
