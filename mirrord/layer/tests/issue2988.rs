#![cfg(target_family = "unix")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2988](https://github.com/metalbear-co/mirrord/issues/2988) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2988(
    #[values(GoVersion::GO_1_24, GoVersion::GO_1_25, GoVersion::GO_1_26)] go_version: GoVersion,
    dylib_path: &Path,
) {
    let (mut test_process, _intproxy) = Application::GoIssue2988(go_version)
        .start_process_with_layer(dylib_path, vec![], None)
        .await;
    test_process.wait_assert_success().await;
}
