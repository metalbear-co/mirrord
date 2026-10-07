#![cfg(target_family = "unix")]

use rstest::rstest;

mod common;
pub use common::*;

/// Verifies that the `sendfile` hook reports a would-block socket the way libc does: -1 with
/// `EAGAIN`, and on macOS `*len` set to the zero bytes actually sent. Callers such as Ruby's
/// `IO.copy_stream` trust `*len` even when `sendfile` fails, so leaving it at the requested length
/// makes them skip data that never reached the socket.
#[rstest]
#[tokio::test]
async fn sendfile_eagain() {
    let _tracing = init_tracing();

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("sendfile_eagain_local_fs.json");
    let config = serde_json::json!({
        "feature": {
            "fs": {
                "mode": "localwithoverrides",
                "local": ["^/tmp/sendfile_eagain_.*"]
            }
        }
    });
    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .await
        .expect("failed to save layer config to tmp file");

    let (mut test_process, mut intproxy) = Application::SendfileEagain
        .start_process(Default::default(), Some(&config_path))
        .await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
