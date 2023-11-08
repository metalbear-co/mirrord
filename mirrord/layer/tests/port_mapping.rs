#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn port_mapping(
    #[values(Application::RustIssue2058)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer_and_port(
            dylib_path,
            vec![],
            Some(config_dir.join("port_mapping.json").to_str().unwrap()),
        )
        .await;

    println!("Application subscribed to port, sending data.");

    intproxy
        .send_connection_then_data("HELLO", application.get_app_port())
        .await;

    test_process.wait_assert_success().await;
}
