#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

use std::time::Duration;

use rstest::rstest;

mod common;
pub use common::*;

/// Verify that the layer's connection to the internal proxy does not take fd 3.
///
/// s6-overlay's `/init` waits for a readiness notification on fd 3 with this exact probe, which
/// blocks forever when fd 3 is the idle proxy socket, so `mirrord container` hangs on any image
/// using s6-overlay with its default entrypoint.
#[rstest]
#[tokio::test]
async fn s6_overlay_init_fd3_probe() {
    let (mut test_process, _intproxy) = Application::DynamicApp(
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            "if read _ 2>/dev/null <&3 ; then exec 3<&- ; fi".to_owned(),
        ],
    )
    .start_process(vec![], None)
    .await;

    tokio::time::timeout(Duration::from_secs(60), test_process.wait_assert_success())
        .await
        .expect("the fd 3 probe blocked on the internal proxy connection");
}
