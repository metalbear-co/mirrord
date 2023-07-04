#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

#[cfg(not(target_os = "macos"))]
use futures::SinkExt;
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
#[cfg(not(target_os = "macos"))]
use tokio_stream::StreamExt;

/// Run a bash script and verify that mirrord is able to load and hook into env, bash and cat.
/// On MacOS, this works because the executable is patched before running, and the calls to
/// `execve` for `bash` and `cat` are hooked, and the binaries are patched.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn bash_script(dylib_path: &Path) {
    let application = Application::EnvBashCat;
    let executable = application.get_executable().await; // Own it.
    println!("Using executable: {}", &executable);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    println!("Listening for messages from the layer on {addr}");
    let env = get_env(dylib_path.to_str().unwrap(), &addr, vec![], None);
    #[cfg(target_os = "macos")]
    let executable = sip_patch(&executable, &Vec::new()).unwrap().unwrap();
    let test_process = TestProcess::start_process(executable, application.get_args(), env).await;

    // Accept the connection from the layer in the env binary and verify initial messages.
    let _env_layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    // Accept the connection from the layer in the bash binary and verify initial messages.
    let mut bash_layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    // Accept the connection from the layer in the cat binary and verify initial messages.

    let fd: u64 = 1;

    bash_layer_connection.expect_gethostname(fd).await;

    // After the process forks we create a new main loop layer task in the child process.
    // That connection will die as soon as the new process calls execve, then a new layer will be
    // initialized.
    let mut _layer_after_fork_before_exec =
        LayerConnection::get_initialized_connection(&listener).await;

    let mut cat_layer_connection = LayerConnection::get_initialized_connection(&listener).await;
    // TODO: theoretically the connections arrival order could be different, should we handle it?

    cat_layer_connection
        .expect_file_open_for_reading("/very_interesting_file", fd)
        .await;

    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(
            cat_layer_connection.codec.next().await.unwrap().unwrap(),
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

        cat_layer_connection
            .codec
            .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                XstatResponse { metadata },
            ))))
            .await
            .unwrap();
    }

    cat_layer_connection
        .expect_file_read("Very interesting contents.", fd)
        .await;

    // don't expect file close as it might not get called due to race condition
    // and that's okay - it's either file closing then process terminates or process terminates.
    // which closes the whole session in the agent

    test_process.assert_no_error_in_stdout().await;
    test_process.assert_no_error_in_stderr().await;
}
