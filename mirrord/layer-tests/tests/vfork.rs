#![cfg(target_family = "unix")]

use rstest::rstest;

mod common;
pub use common::*;

/// Test that hooks work in a child process after a program calls `vfork` without `execve`.
///
/// `vfork` is emulated with a real `fork`, so the child runs the layer's hooks and its file
/// operations reach the internal proxy.
///
/// Note that this does not assert the child holds a *separate* proxy connection: the child
/// inherits the parent's socket across the fork, so its requests arrive either way, and
/// [`TestIntProxy`] accepts a single layer connection. Covering that would mean teaching the
/// harness to accept and distinguish more than one.
#[rstest]
#[tokio::test]
async fn vfork() {
    let application = Application::Vfork;
    let (mut test_process, mut intproxy) =
        application.start_process(Default::default(), None).await;

    println!("waiting for file request.");
    intproxy
        .expect_file_open_with_whatever_options("/path/to/some/file", 1)
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
