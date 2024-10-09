#![feature(assert_matches)]
use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn rebind0(dylib_path: &Path) {
    let application = Application::RustRebind0;
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
