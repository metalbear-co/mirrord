#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::time::Duration;

use rstest::rstest;

mod common;

pub use common::*;

/// Exercises the `getifaddrs`/`freeifaddrs` hooks that the `hide_ipv6_interfaces`
/// experimental feature installs. The C app ([`Application::Getifaddrs`]) walks
/// the whole interface list and reads every pointer reachable from each node, so
/// once ASan is wired into the test suite this catches a use-after-free in the
/// hook's copy of the list.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn getifaddrs(#[values(false, true)] hide_ipv6_interfaces: bool) {
    let application = Application::Getifaddrs;

    let dir = tempfile::tempdir().unwrap();
    let file_id = rand::random::<u64>();
    let config_path = dir.path().join(format!("{file_id:X}.json"));

    let config = serde_json::json!({
        "experimental": {
            "hide_ipv6_interfaces": hide_ipv6_interfaces,
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
        .expect("failed to save layer config to tmp file");

    let (mut test_process, _intproxy) = application
        .start_process(Default::default(), Some(&config_path))
        .await;

    test_process.wait_assert_success().await;

    if hide_ipv6_interfaces {
        test_process
            .assert_stdout_doesnt_contain("family=IPv6")
            .await;
    }
}
