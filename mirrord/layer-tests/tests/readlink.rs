#![cfg(target_family = "unix")]

use rstest::rstest;

use mirrord_layer_tests::*;

/// Test for the [`libc::readlink`] function.
#[rstest]
#[tokio::test]
async fn readlink() {
    let application = Application::ReadLink;

    let (mut test_process, mut intproxy) =
        application.start_process(Default::default(), None).await;

    println!("waiting for file request.");
    intproxy.expect_read_link("/gatos/tigrado.txt").await;

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}
