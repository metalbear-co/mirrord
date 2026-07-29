#![cfg(target_family = "unix")]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Verifies that the `open`-family hooks forward the variadic `mode` argument when they bypass the
/// call, so files created on locally-overridden paths get exactly the permissions the application
/// asked for.
///
/// See [COR-1734](https://linear.app/metalbear/issue/COR-1734).
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn cor_1734_file_mode(config_dir: &Path) {
    let _tracing = init_tracing();
    let application = Application::Cor1734FileMode;
    let config_path = config_dir.join("cor_1734_local_fs.json");

    let (mut test_process, mut intproxy) = application
        .start_process(Default::default(), Some(&config_path))
        .await;

    // Every path the application touches is local, so nothing should reach the agent.
    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
