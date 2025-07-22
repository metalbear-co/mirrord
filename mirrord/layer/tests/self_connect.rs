#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;
use mirrord_protocol::tcp::LayerTcp;

/// Verify that if mirrord application connects to it own listening port it
/// doesn't go through the layer unnecessarily.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn self_connect(dylib_path: &Path) {
    let application = Application::PythonSelfConnect;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer_and_port(dylib_path, vec![("MIRRORD_FILE_MODE", "local")], None)
        .await;
    match intproxy.try_recv().await {
        // Accepting both a PortUnsubscribe and a hangup without it, so that this test does not
        // depend on unrelated implementation details.
        Some(mirrord_protocol::ClientMessage::Tcp(LayerTcp::PortUnsubscribe(_))) | None => {}

        // We want to make sure the layer does not try to connect to itself via the agent by sending
        // messages, but a message was sent by the layer to the agent. If that message is not about
        // the connection, and is valid, you can add it to this match.
        // We're not just accepting any message except for the connection one, so that the test
        // won't just pass by accident if we change the outgoing connection's implementation.
        _ => panic!("Expected the application to exit without sending further messages"),
    }
    assert_eq!(intproxy.try_recv().await, None);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
