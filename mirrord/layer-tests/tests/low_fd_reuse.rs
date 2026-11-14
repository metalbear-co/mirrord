#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

//! Checks that the layer's connection to the internal proxy survives an application that uses
//! low fd numbers without asking for free ones.
//!
//! The application (`apps/low_fd_reuse.py`) overwrites fds 3 to 9 with `dup2`, closes them, and
//! then opens a remote file. When the connection is on one of these fds, the layer loses it, and
//! the remote `open` never reaches the internal proxy.

use std::time::Duration;

use mirrord_intproxy_protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage};
use mirrord_layer_tests::fake_intproxy::FakeIntProxy;
use mirrord_protocol::{
    FileRequest, FileResponse,
    file::{OpenFileRequest, OpenFileResponse},
};
use rstest::rstest;

mod common;

pub use common::*;

/// How long the test waits for something that must happen.
const TIMEOUT: Duration = Duration::from_secs(30);

const REMOTE_FILE: &str = "/mirrord-test/file";

#[rstest]
#[tokio::test]
async fn app_reuses_low_fds() {
    let mut intproxy = FakeIntProxy::new().await;
    let env = get_env(
        intproxy.address(),
        vec![
            ("MIRRORD_FILE_MODE", "localwithoverrides"),
            ("MIRRORD_FILE_READ_WRITE_PATTERN", "^/mirrord-test/"),
        ],
    );
    let app_path = common::manifest_dir().join("tests/apps/low_fd_reuse.py");
    let application = Application::DynamicApp(
        Application::get_python3_executable().await,
        vec!["-u".to_owned(), app_path.to_string_lossy().into_owned()],
    );
    let mut process = application.get_test_process(env, None).await;

    let mut layer = intproxy
        .try_accept(TIMEOUT)
        .await
        .expect("the layer did not connect");
    let remote_fd = loop {
        let message = tokio::time::timeout(TIMEOUT, layer.recv())
            .await
            .expect("the layer sent nothing");
        match message {
            // `python3` can be a wrapper that loads the layer and then calls `exec`, which closes
            // its connection. Then the layer of the interpreter connects again.
            // If no new connection arrives, the layer lost its connection when the app took over fds
            // 3 to 9.
            None => {
                layer = intproxy
                    .try_accept(TIMEOUT)
                    .await
                    .expect("the layer lost its connection before the remote open");
            }
            Some(LocalMessage {
                message_id,
                inner: LayerToProxyMessage::File(FileRequest::Open(OpenFileRequest { path, .. })),
            }) if path == std::path::Path::new(REMOTE_FILE) => {
                layer
                    .send(
                        message_id,
                        ProxyToLayerMessage::File(FileResponse::Open(Ok(OpenFileResponse {
                            fd: message_id,
                        }))),
                    )
                    .await;
                break message_id;
            }
            Some(other) => panic!("unexpected request from the layer: {other:?}"),
        }
    };

    let close = tokio::time::timeout(TIMEOUT, layer.recv())
        .await
        .expect("the layer sent nothing")
        .expect("the layer closed its connection before closing the remote file");
    assert!(
        matches!(
            close.inner,
            LayerToProxyMessage::File(FileRequest::Close(ref request)) if request.fd == remote_fd
        ),
        "expected the close of the remote file, got {close:?}"
    );

    process.wait_assert_success().await;
    process.assert_stdout_contains("remote open done").await;
}
