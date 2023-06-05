#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use rstest::rstest;

mod common;

use std::{path::PathBuf, time::Duration};

pub use common::*;

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn recv_from(
    #[values(Application::RustRecvFrom)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    assert!(layer_connection.is_ended().await);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}
