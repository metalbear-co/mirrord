use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::PollSemaphore;

/// Wrapper over a [`Stream`] of [`Bytes`], that yields items only after only acquiring
/// [`Semaphore`] permit for each byte of data.
///
/// E.g. if the inner stream yields 1024 bytes of data, this wrapper will yield a tuple of (data,
/// 1024 permits).
///
/// Allows for throttling consumption of incoming data.
///
/// # Important
///
/// Mind that if the [`Semaphore`] (passed in [`Self::new`]) never has enough permits to cover an
/// item yielded by the inner stream, [`Stream::poll_next`] implementation of this wrapper will hang
/// forever.
pub struct ThrottledStream<S> {
    inner: S,
    ready_data: Option<Bytes>,
    semaphore: PollSemaphore,
}

impl<S> ThrottledStream<S> {
    pub fn new(inner: S, semaphore: Arc<Semaphore>) -> Self {
        Self {
            inner,
            ready_data: None,
            semaphore: PollSemaphore::new(semaphore),
        }
    }
}

impl<S> Stream for ThrottledStream<S>
where
    S: Stream<Item = io::Result<Bytes>> + Unpin,
{
    type Item = io::Result<(Bytes, OwnedSemaphorePermit)>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let ready_data = match this.ready_data.take() {
            Some(data) => data,
            None => {
                let data = std::task::ready!(this.inner.poll_next_unpin(cx)).transpose()?;
                let Some(data) = data else {
                    return Poll::Ready(None);
                };
                data
            }
        };

        let permits = u32::try_from(ready_data.len()).unwrap_or(u32::MAX);
        match this.semaphore.poll_acquire_many(cx, permits) {
            Poll::Ready(Some(permits)) => Poll::Ready(Some(Ok((ready_data, permits)))),
            Poll::Ready(None) => Poll::Ready(Some(Err(io::Error::other(
                "throttling semaphore is closed",
            )))),
            Poll::Pending => {
                this.ready_data.replace(ready_data);
                Poll::Pending
            }
        }
    }
}
