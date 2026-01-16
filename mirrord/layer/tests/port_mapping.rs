#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

use serde_json::json;

use mirrord_tests::utils::ManagedTempFile;

mod common;
pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn port_mapping(
    #[values(Application::RustIssue2058)] application: Application,
    dylib_path: &Path,
    config_dir: &Path,
) {
    let config_json = json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "mirror",
                    "port_mapping": [[9999, 1234]]]
                }
            }
        }
    });
    let tempfile = ManagedTempFile::new(config_json);
    let config_path = config_dir.join(&tempfile.path); 

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![],
            Some(&config_path),
        )
        .await;

    println!("Application subscribed to port, sending data.");

    intproxy
        .send_connection_then_data("HELLO", application.get_app_port())
        .await;

    test_process.wait_assert_success().await;
}
