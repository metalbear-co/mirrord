#![cfg(target_os = "linux")]

mod common;
pub use common::*;

#[tokio::test]
async fn posix_spawn_children_receive_shared_socket_metadata() {
    let application = Application::PosixSpawnSharedSockets;
    let (mut test_process, _intproxy) = application.start_process(Default::default(), None).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
