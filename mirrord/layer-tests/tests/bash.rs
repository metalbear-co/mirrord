#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

#[cfg(not(target_os = "macos"))]
use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
    file::{MetadataInternal, XstatRequest, XstatResponse},
};
use rstest::rstest;

mod common;

pub use common::*;

/// Run a bash script and verify that mirrord is able to load and hook into env, bash and cat.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn bash_script(config_dir: &Path) {
    let mut config_path = config_dir.to_path_buf();
    // use a config file since cat sometimes opens some weird paths
    // before opening the file we want to read, and it makes testing easier
    // to ignore those paths.
    config_path.push("bash_script.json");

    let (test_process, mut intproxy) = Application::EnvBashCat
        .start_process(vec![], Some(&config_path))
        .await;

    let fd: u64 = 1;

    intproxy.expect_gethostname(fd).await;

    intproxy
        .expect_file_open_for_reading("/very_interesting_file", fd)
        .await;

    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(
            intproxy.recv().await,
            ClientMessage::FileRequest(FileRequest::Xstat(XstatRequest {
                path: None,
                fd: Some(fd),
                follow_symlink: true
            }))
        );

        let metadata = MetadataInternal {
            size: 100,
            blocks: 2,
            ..Default::default()
        };

        intproxy
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await;
    }

    intproxy
        .expect_file_read("Very interesting contents.", fd)
        .await;

    intproxy.expect_file_close(fd).await;

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
