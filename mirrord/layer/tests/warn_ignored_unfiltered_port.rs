#![feature(assert_matches)]

mod common;

use std::{path::Path, time::Duration};

pub use common::*;
use rstest::rstest;

/// If a user wants to HTTP-filter port x, but steal the whole port y, they have to include port y
/// in `feature.network.incoming.ports`. As it's easy to overlook, we want to warn users if we
/// detect a situation where a user is filtering HTTP, and their application then listens on a port
/// that is not included in the filtered ports, and they did not specify
/// `feature.network.incoming.ports` at all.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn warn_ignored_unfiltered_port(dylib_path: &Path, config_dir: &Path) {
    let config_path = config_dir.join("http_filter_port_3000.json");

    // reusing existing application for this test. This test is unrelated to issue 2058.
    let application = Application::RustIssue2058;

    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, vec![], Some(&config_path))
        .await;

    test_process
        .wait_for_line(
            Duration::from_secs(10),
            "Port 9999 was not included in the filtered ports",
        )
        .await;

    test_process.child.kill().await.unwrap();
}
