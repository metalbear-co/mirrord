#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{time::Duration, path::Path};
use rstest::rstest;

mod common;

pub use common::*;

/// Verify that issue [#2988](https://github.com/metalbear-co/mirrord/issues/2988) is fixed.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue2988(
    #[values(Application::Go23Issue2988)] application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;
    test_process.wait_assert_success().await;
}
