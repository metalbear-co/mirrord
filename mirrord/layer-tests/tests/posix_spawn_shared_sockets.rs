#![cfg(target_os = "linux")]

use rstest::rstest;

mod common;
pub use common::*;

#[rstest]
#[case("sequential")]
#[case("concurrent")]
#[tokio::test]
async fn posix_spawn_children_receive_shared_socket_metadata(#[case] mode: &str) {
    let application = Application::PosixSpawnSharedSockets(mode.to_owned());
    let (mut test_process, _) = application
        .start_process_with_port(Default::default(), None)
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
