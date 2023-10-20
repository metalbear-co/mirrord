#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::PathBuf, time::Duration};

use mirrord_protocol::{tcp::LayerTcp, ClientMessage};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#1123](https://github.com/metalbear-co/mirrord/issues/1123) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue1123(
    #[values(Application::RustIssue1123)] application: Application,
    dylib_path: &PathBuf,
) {
    let port = application.get_app_port();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![
                ("RUST_LOG", "mirrord=trace"),
                ("MIRRORD_FILE_MODE", "local"),
                ("MIRRORD_UDP_OUTGOING", "false"),
                ("MIRRORD_REMOTE_DNS", "false"),
            ],
            None,
        )
        .await;

    println!("Application subscribed to port.");

    let msg = intproxy.recv().await;
    assert_eq!(msg, ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)));

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 1123: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 1123: SUCCESS")
        .await;
}
