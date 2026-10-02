#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

//! Checks what the other threads of an application can do while a close hook waits for the
//! intproxy.
//!
//! The application (`apps/close_while_blocked.py`) starts a large remote write, which blocks
//! because the test does not read the intproxy connection of the layer. Then a second thread
//! closes something remote, so the close hook waits for that connection in the middle of its close
//! request. Then the main thread does an action, and the test checks what happens.

use std::{path::PathBuf, time::Duration};

use mirrord_intproxy_protocol::{
    IncomingRequest, IncomingResponse, LayerToProxyMessage, LocalMessage, ProxyToLayerMessage,
};
use mirrord_layer_tests::fake_intproxy::{FakeIntProxy, FakeLayerConnection};
use mirrord_protocol::{
    FileRequest, FileResponse,
    file::{OpenDirResponse, OpenFileResponse, WriteFileResponse},
};
use rstest::rstest;

mod common;

pub use common::*;

/// How long the test waits for something that must happen.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How long the test waits for something that must not happen. This time starts when the
/// application prints that it starts its action.
const NOTHING_HAPPENS: Duration = Duration::from_secs(1);

/// Starts the application with the given `action` and close `target`, and answers its requests
/// until it has opened the remote file of the large write. After this, the test does not read the
/// connection until it has checked the action.
async fn start(action: &str, target: &str) -> (TestProcess, FakeIntProxy, FakeLayerConnection) {
    let mut intproxy = FakeIntProxy::new().await;
    let env = get_env(
        intproxy.address(),
        vec![
            ("MIRRORD_FILE_MODE", "localwithoverrides"),
            ("MIRRORD_FILE_READ_WRITE_PATTERN", "^/mirrord-test/"),
        ],
    );
    let app_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/apps/close_while_blocked.py");
    let application = Application::DynamicApp(
        Application::get_python3_executable().await,
        vec![
            "-u".to_owned(),
            app_path.to_string_lossy().into_owned(),
            action.to_owned(),
            target.to_owned(),
        ],
    );
    let process = application.get_test_process(env, None).await;

    let mut layer = accept(&mut intproxy).await;
    loop {
        match answer(&mut layer).await {
            // `python3` can be a wrapper that loads the layer and then calls `exec`, which closes
            // its connection. Then the layer of the interpreter connects again.
            None => layer = accept(&mut intproxy).await,
            Some(LayerToProxyMessage::File(FileRequest::Open(open)))
                if open.path.ends_with("blocker") =>
            {
                break;
            }
            Some(_) => {}
        }
    }

    (process, intproxy, layer)
}

async fn accept(intproxy: &mut FakeIntProxy) -> FakeLayerConnection {
    intproxy
        .try_accept(TIMEOUT)
        .await
        .expect("the layer did not connect")
}

/// Reads the next request of the layer, answers it, and gives it back. Gives [`None`] when the
/// layer closed the connection.
async fn answer(layer: &mut FakeLayerConnection) -> Option<LayerToProxyMessage> {
    let LocalMessage { message_id, inner } = tokio::time::timeout(TIMEOUT, layer.recv())
        .await
        .expect("the layer sent nothing")?;

    let response = match &inner {
        LayerToProxyMessage::Incoming(IncomingRequest::PortSubscribe(_)) => Some(
            ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(()))),
        ),
        LayerToProxyMessage::File(FileRequest::Open(_)) => Some(ProxyToLayerMessage::File(
            FileResponse::Open(Ok(OpenFileResponse { fd: message_id })),
        )),
        LayerToProxyMessage::File(FileRequest::FdOpenDir(_)) => Some(ProxyToLayerMessage::File(
            FileResponse::OpenDir(Ok(OpenDirResponse { fd: message_id })),
        )),
        LayerToProxyMessage::File(FileRequest::Write(write)) => Some(ProxyToLayerMessage::File(
            FileResponse::Write(Ok(WriteFileResponse {
                written_amount: write.write_bytes.len() as u64,
            })),
        )),
        LayerToProxyMessage::File(FileRequest::CloseDir(_))
        | LayerToProxyMessage::Incoming(IncomingRequest::PortUnsubscribe(_)) => None,
        other => panic!("unexpected request from the layer: {other:?}"),
    };
    if let Some(response) = response {
        layer.send(message_id, response).await;
    }

    Some(inner)
}

/// Whether `request` is the close request of the second thread of the application.
fn is_close_request(request: &LayerToProxyMessage) -> bool {
    matches!(
        request,
        LayerToProxyMessage::File(FileRequest::CloseDir(_))
            | LayerToProxyMessage::Incoming(IncomingRequest::PortUnsubscribe(_))
    )
}

/// Answers the requests of the layer until it closes the connection.
async fn answer_all(layer: &mut FakeLayerConnection) {
    while answer(layer).await.is_some() {}
}

/// Checks that the close in the second thread has not finished, so it still waits for the
/// intproxy. Otherwise the test does not check anything.
async fn assert_close_waits(process: &TestProcess) {
    assert!(
        !process.get_stdout().await.contains("close done"),
        "the close did not wait for the intproxy"
    );
}

/// A `fork` waits until a close on another thread has sent its close request. Otherwise the
/// intproxy copies the closed resource of the parent to the child, and nothing closes that copy.
#[rstest]
#[case::closedir("dir")]
#[case::socket("socket")]
#[tokio::test]
async fn fork_waits_for_close(#[case] target: &str) {
    let (mut process, mut intproxy, mut parent) = start("fork", target).await;

    process.wait_for_line_stdout(TIMEOUT, "fork start").await;
    assert_close_waits(&process).await;
    assert!(
        intproxy.try_accept(NOTHING_HAPPENS).await.is_none(),
        "the child connected while the parent was in the middle of a close"
    );

    // The parent sends its close request before the child connects.
    loop {
        let request = answer(&mut parent)
            .await
            .expect("the parent closed the connection");
        if is_close_request(&request) {
            break;
        }
    }
    let mut child = accept(&mut intproxy).await;
    assert_eq!(child.session.parent_layer, Some(parent.id));
    answer_all(&mut child).await;

    process.wait_for_line_stdout(TIMEOUT, "fork done").await;
    answer_all(&mut parent).await;
    process.wait_assert_success().await;
}

/// `posix_spawn` does not wait for a close on another thread. The exec hooks lock `SOCKETS`, so a
/// close must not hold that lock during its request to the intproxy.
#[rstest]
#[case::socket("socket")]
#[tokio::test]
async fn spawn_does_not_wait_for_close(#[case] target: &str) {
    let (mut process, _intproxy, mut layer) = start("spawn", target).await;

    process.wait_for_line_stdout(TIMEOUT, "spawn done").await;
    assert_close_waits(&process).await;

    answer_all(&mut layer).await;
    process.wait_assert_success().await;
}
