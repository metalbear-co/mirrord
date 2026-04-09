//! Util functions for resolving [`Future`]s with a time limit.

use std::{
    panic::Location,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use pin_project_lite::pin_project;
use thiserror::Error;
use tokio::time::{Instant, Timeout};

#[cfg(test)]
mod test;

/// Fancy wrapper for [`tokio::time::timeout`].
///
/// Maps [`tokio::time::error::Elapsed`] into [`Elapsed`],
/// which preserves callsite location thanks to `#[track_caller]` annotation on this function.
#[track_caller]
pub fn rich_timeout<F: IntoFuture>(duration: Duration, future: F) -> RichTimeout<F::IntoFuture> {
    RichTimeout {
        inner: tokio::time::timeout(duration, future),
        duration,
        location: Location::caller(),
    }
}

/// Fancy wrapper for [`tokio::time::timeout_at`].
///
/// Maps [`tokio::time::error::Elapsed`] into [`Elapsed`],
/// which preserves callsite location thanks to `#[track_caller]` annotation on this function.
#[track_caller]
pub fn rich_timeout_at<F: IntoFuture>(deadline: Instant, future: F) -> RichTimeout<F::IntoFuture> {
    let duration = deadline.saturating_duration_since(Instant::now());
    RichTimeout {
        inner: tokio::time::timeout_at(deadline, future),
        duration,
        location: Location::caller(),
    }
}

pin_project! {
    /// [`Future`] returned from [`rich_timeout`] and [`rich_timeout_at`].
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct RichTimeout<F> {
        #[pin]
        inner: Timeout<F>,
        duration: Duration,
        location: &'static Location<'static>,
    }
}

impl<F: Future> Future for RichTimeout<F> {
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.inner.poll(cx).map_err(|_elapsed| Elapsed {
            duration: *this.duration,
            location: this.location,
        })
    }
}

#[derive(Error, Debug, Clone, Copy)]
#[error(
    "timed out after {:.1}s ({}:{})",
    duration.as_secs_f32(),
    location.file(),
    location.line(),
)]
pub struct Elapsed {
    duration: Duration,
    location: &'static Location<'static>,
}
