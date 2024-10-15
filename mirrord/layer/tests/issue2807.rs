#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp},
    ClientMessage, DaemonMessage,
};
use rand::Rng;
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2807](https://github.com/metalbear-co/mirrord/issues/2807) still fails user application if
/// [`ExperimentalConfig::hide_ipv6_interfaces`](mirrord_config::experimental::ExperimentalConfig::hide_ipv6_interfaces) is not set.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2807_without_ipv6_ignore(
    #[values(Application::NodeIssue2807)] application: Application,
    dylib_path: &Path,
) {
    let dir = tempfile::tempdir().unwrap();
    let file_id = rand::thread_rng().gen::<u64>();
    let config_path = dir.path().join(format!("{file_id:X}.json"));

    let config = serde_json::json!({
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

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            Default::default(),
            Some(config_path.to_str().unwrap()),
        )
        .await;

    match intproxy.recv().await {
        ClientMessage::Tcp(LayerTcp::PortSubscribe(port)) => {
            intproxy
                .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
                .await;
        }
        other => panic!("unexpected message received from intproxy: {other:?}"),
    };

    let handle = tokio::spawn(handle_port_subscriptions(intproxy));

    test_process.wait_assert_fail().await;
    handle.await.unwrap();
}

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
    let file_id = rand::thread_rng().gen::<u64>();
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

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            Default::default(),
            Some(config_path.to_str().unwrap()),
        )
        .await;

    match intproxy.recv().await {
        ClientMessage::Tcp(LayerTcp::PortSubscribe(port)) => {
            intproxy
                .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))))
                .await;
        }
        other => panic!("unexpected message received from intproxy: {other:?}"),
    };

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
            other => panic!("unexpected message received from intproxy: {other:?}"),
        }
    }
}
