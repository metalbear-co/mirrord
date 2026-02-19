#![cfg(target_family = "unix")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use rstest::rstest;

mod common;

pub use common::*;
use tempfile::NamedTempFile;

/// Verify that issue [#3456](https://github.com/metalbear-co/mirrord/issues/3456) properly hooks `rename`.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_issue3456(
    #[values(Application::NodeIssue3456)] application: Application,
    dylib_path: &Path,
) {
    use std::{
        fs::{self, File},
        io::Write,
    };

    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();

    let config = serde_json::json!({
        "experimental": {
            "hook_rename": true
        },
        "feature": {
            "fs": {
                "mode": "localwithoverrides",
                "read_write": "/tmp/krakus.*",
                "local": ["/tmp/leszko.*", "/tmp/piast.pol"],
                "mapping": { "/tmp/piast.pol": "/tmp/wanda.pol"},
            }
        }
    });
    config_file
        .as_file_mut()
        .write_all(config.to_string().as_bytes())
        .unwrap();

    let _ = File::create("/tmp/leszko_i.pol").unwrap();
    let _ = File::create("/tmp/wanda.pol").unwrap();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), Some(config_file.path()))
        .await;

    intproxy
        .expect_file_rename("/tmp/krakus_i.pol", "/tmp/krakus_ii.pol")
        .await;

    test_process.wait_assert_success().await;

    test_process
        .assert_stdout_contains("test issue 3456: SUCCESS")
        .await;

    fs::remove_file("/tmp/leszko_ii.pol")
        .expect("leszko_i.pol should be renamed locally to leszko_ii.pol");
    fs::remove_file("/tmp/lech.pol")
        .expect("piast.pol should be mapped to wanda.pol, which then gets renamed to lech.pol");
}
