#![cfg(target_os = "macos")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn skip_sip(dylib_path: &Path, config_dir: &Path) {
    let application = Application::RustIssue3248;
    let config_path = config_dir.join("skip_sip.json");
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("RUST_LOG", "mirrord=trace")],
            Some(&config_path),
        )
        .await;

    test_process.wait_assert_success().await;
}
