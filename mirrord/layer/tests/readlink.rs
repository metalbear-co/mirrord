#![feature(assert_matches)]
use std::{path::PathBuf, time::Duration};

use rstest::rstest;

mod common;
pub use common::*;

/// Test for the [`libc::readlink`] function.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn readlink(
    #[values(Some("readlink.json"))] with_config: Option<&str>,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let application = Application::ReadLink;

    let config = with_config.map(|config| {
        let mut config_path = config_dir.clone();
        config_path.push(config);
        config_path
    });
    let config = config.as_ref().map(|path_buf| path_buf.to_str().unwrap());

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), config)
        .await;

    println!("waiting for file request.");
    intproxy.expect_read_link("/gatos/tigrado.txt").await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
