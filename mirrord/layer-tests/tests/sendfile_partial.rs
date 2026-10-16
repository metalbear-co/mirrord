#![cfg(target_os = "linux")]

use rstest::rstest;

mod common;
pub use common::*;

/// Verifies that the `sendfile` hook counts only the bytes it actually wrote when the output
/// accepts fewer bytes than were read.
/// `*offset` advances by the bytes written, and with a null offset the source file position
/// does too.
#[rstest]
#[tokio::test]
async fn sendfile_partial() {
    let _tracing = init_tracing();

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("sendfile_partial_local_fs.json");
    let config = serde_json::json!({
        "feature": {
            "fs": {
                "mode": "localwithoverrides",
                "local": ["^/tmp/sendfile_partial_.*"]
            }
        }
    });
    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .await
        .expect("failed to save layer config to tmp file");

    let (mut test_process, mut intproxy) = Application::SendfilePartial
        .start_process(Default::default(), Some(&config_path))
        .await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
