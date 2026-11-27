#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// A .NET runtime pauses at its startup on the diagnostic port `mirrord up` serves, a local
/// socket. Its connection stays local even when `unix_streams` matches every path, or the runtime
/// would wait on the pod forever.
#[rstest]
#[tokio::test]
async fn dotnet_diagnostic_port_stays_local(config_dir: &Path) {
    let config = config_dir.join("unix_streams_all.toml");
    let (mut test_process, mut intproxy) = Application::PythonDotnetDiagnosticPort
        .start_process(vec![], Some(&config))
        .await;

    if let Ok(Some(msg)) = tokio::time::timeout(Duration::from_secs(5), intproxy.try_recv()).await {
        panic!("the connection to the diagnostic port reached the intproxy: {msg:?}");
    }

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("connected locally")
        .await;
}
