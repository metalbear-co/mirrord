//! Tests for [`super::rich_timeout`].
//!
//! Placed in a separate file becaues [`super::Elapsed`] contains info about the
//! callsite, which one of the tests verifies.

use std::time::Duration;

/// Verifies that [`super::rich_timeout`] returns a correct error if the timeout elapses.
#[tokio::test]
async fn elapsed() {
    let error = super::rich_timeout(Duration::from_millis(50), std::future::pending::<()>())
        .await
        .unwrap_err();
    assert_eq!(error.duration, Duration::from_millis(50));
    assert_eq!(error.location.file(), file!()); // should be this file, not the one where `rich_timeout` is implemented
    assert_eq!(error.location.line(), line!() - 5);
}

/// Verifies that [`super::rich_timeout`] returns the inner [`Future`]'s result, if it completes
/// within the time limit.
#[tokio::test]
async fn completed() {
    super::rich_timeout(
        Duration::from_millis(50),
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await
    .unwrap();
}
