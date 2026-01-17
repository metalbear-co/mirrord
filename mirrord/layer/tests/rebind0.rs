#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

use serde_json::json;

use mirrord_tests::utils::ManagedTempFile;

mod common;

pub use common::*;

/// Run an application that binds 0.0.0.0:0 twice and verify:
/// 1. A port is bound successfully both times (the app does not panic).
/// 2. No warning is displayed.
///
/// Both of those things used to happen, and this test verifies there is no regression to that.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn rebind0(dylib_path: &Path, config_dir: &Path) {
    let mut config_path = config_dir.to_path_buf();

    // This configuration used to trigger a false warning, it is not used for any functionality,
    // just to make sure it does not result in a warning anymore.
    let config_json = json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "steal",
                    "http_filter": {
                        "header_filter": "this is just to trigger a wrong warning log, there are not requests in the test"
                    }
                }
            }
        }
    });
    let tempfile = ManagedTempFile::new(config_json);
    config_path.push(&tempfile.path);

    let application = Application::RustRebind0;
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, vec![], Some(&config_path))
        .await;

    // Before https://github.com/metalbear-co/mirrord/pull/2811, this would panic.
    test_process.wait_assert_success().await;

    // There used to be a wrong warning "Port 0 was not included in the filtered ports..."
    test_process.assert_no_warn_in_stderr().await;

    test_process.assert_no_error_in_stderr().await;
}
