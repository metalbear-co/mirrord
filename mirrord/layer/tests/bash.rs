#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(not(target_os = "macos"))]
use mirrord_protocol::{
    file::{MetadataInternal, XstatRequest, XstatResponse},
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
};
#[cfg(target_os = "macos")]
use mirrord_sip::sip_patch;
use rstest::rstest;
use tokio::net::TcpListener;

mod common;

pub use common::*;

/// Run a bash script and verify that mirrord is able to load and hook into env, bash and cat.
/// On MacOS, this works because the executable is patched before running, and the calls to
/// `execve` for `bash` and `cat` are hooked, and the binaries are patched.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn bash_script(dylib_path: &Path, config_dir: &PathBuf) {
    let mut config_path = config_dir.clone();
    // use a config file since cat sometimes opens some weird paths
    // before opening the file we want to read, and it makes testing easier
    // to ignore those paths.
    config_path.push("bash_script.json");
    let application = Application::EnvBashCat;
    let executable = application.get_executable().await; // Own it.
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(
        dylib_path.to_str().unwrap(),
        &addr,
        vec![],
        Some(config_path.to_str().unwrap()),
    );
    #[cfg(target_os = "macos")]
    let executable = sip_patch(&executable, &Vec::new()).unwrap().unwrap();
    let test_process = TestProcess::start_process(executable, application.get_args(), env).await;

    let mut intproxy = TestIntProxy::new(listener).await;

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
