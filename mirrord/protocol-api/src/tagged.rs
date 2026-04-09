use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{FutureExt, Stream, StreamExt};
use pin_project_lite::pin_project;

pin_project! {
    /// A [`Stream`] or [`Future`] wrapper that adds a tag to each resolved item.
    pub struct Tagged<I, T> {
        #[pin]
        pub inner: I,
        pub tag: T,
    }
}

impl<I, T> Stream for Tagged<I, T>
where
    I: Stream,
    T: Clone,
{
    type Item = (T, I::Item);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        this.inner
            .poll_next_unpin(cx)
            .map(|item| item.map(|item| (this.tag.clone(), item)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<I, T> Future for Tagged<I, T>
where
    I: Future,
    T: Clone,
{
    type Output = (T, I::Output);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        this.inner
            .poll_unpin(cx)
            .map(|output| (this.tag.clone(), output))
    }
}
