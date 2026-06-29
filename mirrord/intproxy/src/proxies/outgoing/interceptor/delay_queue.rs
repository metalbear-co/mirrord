//! A FIFO queue of payloads that each become due at a deadline.
//!
//! Used by the outgoing interceptor's [read](super::read_queue) and [write](super::write_queue)
//! queues to release messages after a chaos latency delay.
//!
//! This queue yields strictly in insertion order: it only ever waits on the *front* item's
//! deadline. This is required because the chaos latency carries jitter. Releasing in deadline order
//! would reorder the byte stream of an intercepted connection.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::time::{Instant, Sleep, sleep_until};
use tokio_stream::Stream;

/// A payload paired with the [`Instant`] at which it becomes due to be sent.
struct Delayed<T> {
    payload: T,
    deadline: Instant,
}

/// FIFO queue that yields each payload no earlier than its deadline, in insertion order.
pub(super) struct DelayQueue<T> {
    items: VecDeque<Delayed<T>>,

    /// Timer that wakes the task at the current front item's deadline.
    ///
    /// Kept as state (rather than recreated on every poll) so the waker it registers survives
    /// across polls.
    timer: Option<Pin<Box<Sleep>>>,
}

impl<T> Default for DelayQueue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            timer: None,
        }
    }
}

impl<T> DelayQueue<T> {
    /// Enqueues `payload` to be yielded by [`Stream::poll_next`] once `deadline` is reached.
    pub(super) fn push(&mut self, payload: T, deadline: Instant) {
        self.items.push_back(Delayed { payload, deadline });
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl<T: Unpin> Stream for DelayQueue<T> {
    type Item = T;

    /// Yields the front payload once its deadline has passed.
    ///
    /// An already-due front is popped and returned immediately, with no timer involved, so a burst
    /// of due messages drains at line rate (this is what keeps the latency from accumulating per
    /// message). A not-yet-due front arms a [`Sleep`] purely to register a wake at its deadline,
    /// then returns [`Poll::Pending`].
    ///
    /// When empty, returns [`Poll::Ready`]`(None)` which does NOT mean end-of-stream. During the
    /// live session callers guard against polling an empty queue (the sibling `select!` branch
    /// feeding [`Self::push`] makes progress instead); the `None` is what terminates the final
    /// drain once the input channel has closed.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let deadline = match this.items.front() {
            Some(front) => front.deadline,
            None => {
                this.timer = None;
                return Poll::Ready(None);
            }
        };

        if deadline <= Instant::now() {
            this.timer = None;
            match this.items.pop_front() {
                Some(front) => return Poll::Ready(Some(front.payload)),
                None => {
                    return Poll::Ready(None);
                }
            };
        }

        let timer = this
            .timer
            .get_or_insert_with(|| Box::pin(sleep_until(deadline)));
        let _ = timer.as_mut().poll(cx);

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;
    use tokio_stream::StreamExt;

    use super::DelayQueue;

    #[tokio::test(start_paused = true)]
    async fn honors_front_deadline() {
        let start = Instant::now();
        let mut queue = DelayQueue::default();
        queue.push("a", start + Duration::from_secs(2));

        assert_eq!(queue.next().await, Some("a"));
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn yields_in_insertion_order_under_non_monotonic_deadlines() {
        let start = Instant::now();
        let mut queue = DelayQueue::default();
        // Enqueued first but due later.
        queue.push("first", start + Duration::from_secs(3));
        // Enqueued second but due sooner.
        queue.push("second", start + Duration::from_secs(1));

        assert_eq!(queue.next().await, Some("first"));
        assert_eq!(start.elapsed(), Duration::from_secs(3));

        assert_eq!(queue.next().await, Some("second"));
        assert_eq!(start.elapsed(), Duration::from_secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn same_deadline_drains_in_order_without_accumulating() {
        let start = Instant::now();
        let mut queue = DelayQueue::default();
        let deadline = start + Duration::from_secs(1);
        for i in 0u8..3 {
            queue.push(i, deadline);
        }

        assert_eq!(queue.next().await, Some(0));
        // Waited once for the shared deadline.
        assert_eq!(start.elapsed(), Duration::from_secs(1));

        assert_eq!(queue.next().await, Some(1));
        assert_eq!(queue.next().await, Some(2));
        // The rest came out with no additional waiting.
        assert_eq!(start.elapsed(), Duration::from_secs(1));

        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn empty_queue_yields_none() {
        let mut queue = DelayQueue::<u8>::default();

        assert!(queue.is_empty());
        assert_eq!(queue.next().await, None);
    }
}
