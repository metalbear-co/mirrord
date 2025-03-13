#![cfg(target_os = "macos")] // linux github runners don't have ipv6, which we require for these tests
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use mirrord_protocol::{
    file::{OpenFileResponse, ReadFileResponse},
    tcp::{DaemonTcp, LayerTcp},
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2807](https://github.com/metalbear-co/mirrord/issues/2807) does not fail user application if
/// [`ExperimentalConfig::hide_ipv6_interfaces`](mirrord_config::experimental::ExperimentalConfig::hide_ipv6_interfaces) is set.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2807_with_ipv6_ignore(
    #[values(Application::NodeIssue2807)] application: Application,
    dylib_path: &Path,
) {
    let dir = tempfile::tempdir().unwrap();
    let file_id = rand::random::<u64>();
    let config_path = dir.path().join(format!("{file_id:X}.json"));

    let config = serde_json::json!({
        "experimental": {
            "hide_ipv6_interfaces": true
        },
        "feature": {
            "network": {
                "dns": false
            },
            "fs": "local",
        }
    });

    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .await
        .expect("failed to saving layer config to tmp file");

    let (mut test_process, intproxy) = application
        .start_process_with_layer(
            dylib_path,
            Default::default(),
            Some(&config_path),
        )
        .await;

    let handle = tokio::spawn(handle_port_subscriptions(intproxy));

    test_process.wait_assert_success().await;
    test_process.assert_stdout_contains("Found port").await;
    handle.await.unwrap();
}

async fn handle_port_subscriptions(mut intproxy: TestIntProxy) {
    while let Some(msg) = intproxy.try_recv().await {
        match msg {
            ClientMessage::Tcp(LayerTcp::PortSubscribe(port)) => {
                intproxy
                    .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
                    .await;
            }
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(..)) => {}
            ClientMessage::FileRequest(FileRequest::Open(open)) => {
                assert_eq!(open.path, PathBuf::from("/etc/hostname"));
                intproxy
                    .send(DaemonMessage::File(FileResponse::Open(Ok(
                        OpenFileResponse { fd: 1222 },
                    ))))
                    .await;
            }
            ClientMessage::FileRequest(FileRequest::Close(close)) => {
                assert_eq!(close.fd, 1222);
            }
            ClientMessage::FileRequest(FileRequest::Read(read)) => {
                assert_eq!(read.remote_fd, 1222);
                let hostname = b"mirrord-dummy";
                intproxy
                    .send(DaemonMessage::File(FileResponse::Read(Ok(
                        ReadFileResponse {
                            bytes: hostname.into(),
                            read_amount: hostname.len() as u64,
                        },
                    ))))
                    .await;
            }
            other => panic!("unexpected message received from intproxy: {other:?}"),
        }
    }
}
