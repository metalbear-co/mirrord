#![cfg(target_os = "macos")] // linux github runners don't have ipv6, which we require for these tests
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rand::Rng;
use rstest::rstest;

mod common;

pub use common::*;

/// Related to issue [#2903](https://github.com/metalbear-co/mirrord/issues/2903)
///
/// Verifies the IPv6 interfaces are hidden when feature `hide_ipv6_interfaces` is used
/// via a call to os.networkInterfaces(), which calls C function getifaddrs()
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn check_ipv6s_hidden_with_config(
    #[values(Application::NodeIssue2903)] application: Application,
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
        .expect("failed to save layer config to tmp file");

    let (mut test_process, _intproxy) = application
        .start_process_with_layer(
            dylib_path,
            Default::default(),
            Some(config_path.to_str().unwrap()),
        )
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_doesnt_contain("family: 'IPv6'")
        .await;
}
