use std::{
    collections::VecDeque,
    fmt, io,
    num::NonZeroUsize,
    ops::Not,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use futures::{Sink, Stream};
use thiserror::Error;

use crate::{shrinkable::Shrinkable, size::HeapSize};

/// Unidirectional first-in-first-out queue of items with known size.
///
/// # Capacity
///
/// The capacity of the queue is specified as the total *size* of items, not their count.
/// Item size is specified when pushing an item to the queue.
///
/// To ensure that any item can be sent to the queue, capacity limit is ignored when sending to an
/// empty queue.
#[derive(Debug)]
pub struct Fifo<T> {
    /// For sending items.
    pub sink: FifoSink<T>,
    /// For receiving items.
    pub stream: FifoStream<T>,
    /// For waiting until the fifo is closed.
    pub closed: FifoClosed<T>,
}

impl<T> Fifo<T> {
    /// Creates a new queue with the given capacity.
    pub fn with_capacity(capacity_bytes: NonZeroUsize) -> Self {
        let state = FifoState {
            items: Default::default(),
            size_bytes: 0,
            capacity_bytes: capacity_bytes.get(),
            waits_can_send: Default::default(),
            waits_can_recv: Default::default(),
            waits_is_closed: Default::default(),
        };
        let state = SharedFifoState(Arc::new(Mutex::new(state)));
        Self {
            sink: FifoSink {
                state: state.clone(),
                pending: None,
            },
            stream: FifoStream(state.clone()),
            closed: FifoClosed(state),
        }
    }
}

/// An error that occurs when sending to a closed [`FifoSink`].
#[derive(Error, Debug, Clone, Copy)]
#[error("fifo is closed")]
pub struct FifoClosedError;

impl From<FifoClosedError> for io::Error {
    fn from(_: FifoClosedError) -> Self {
        Self::new(
            io::ErrorKind::ConnectionReset,
            "mirrord-protocol API traffic fifo is closed",
        )
    }
}

/// For sending items to a [`Fifo`], implements [`Sink`].
///
/// Dropping it will close the fifo.
#[derive(Debug)]
pub struct FifoSink<T> {
    state: SharedFifoState<T>,
    pending: Option<WithSize<T>>,
}

impl<T> FifoSink<T> {
    /// If this sink has an unflushed item, takes and returns it.
    pub fn take_pending(&mut self) -> Option<T> {
        self.pending.take()?.item.into()
    }
}

impl<T: HeapSize + Unpin> Sink<T> for FifoSink<T> {
    type Error = FifoClosedError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        let this = self.get_mut();
        let item = WithSize {
            size: std::mem::size_of_val(&item) + item.heap_size(),
            item,
        };
        if this.pending.replace(item).is_some() {
            panic!("FifoSink not ready")
        }
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let Some(pending) = this.pending.take() else {
            return Poll::Ready(Ok(()));
        };
        let mut guard = this.state.0.lock().unwrap();
        match guard.try_push(pending, cx.waker())? {
            Some(item) => {
                this.pending = Some(item);
                Poll::Pending
            }
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let mut guard = this.state.0.lock().unwrap();

        if let Some(pending) = this.pending.take()
            && let Some(item) = guard.try_push(pending, cx.waker())?
        {
            this.pending = Some(item);
            return Poll::Pending;
        }

        guard.close();
        Poll::Ready(Ok(()))
    }
}

impl<T> Drop for FifoSink<T> {
    fn drop(&mut self) {
        self.state.0.lock().unwrap().close();
    }
}

/// For receiving items from a [`Fifo`], implements [`Stream`].
///
/// Dropping it will close the fifo **and** drop all items inside of it.
#[derive(Debug)]
pub struct FifoStream<T>(SharedFifoState<T>);

impl<T> Stream for FifoStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().0.0.lock().unwrap().try_pop(cx.waker()) {
            Ok(Some(item)) => Poll::Ready(Some(item)),
            Ok(None) => Poll::Pending,
            Err(..) => Poll::Ready(None),
        }
    }
}

impl<T> Drop for FifoStream<T> {
    fn drop(&mut self) {
        let mut guard = self.0.0.lock().unwrap();
        guard.close();
        guard.items = Default::default();
    }
}

/// [`Future`] that resolves when a [`Fifo`] is closed.
#[derive(Debug)]
pub struct FifoClosed<T>(SharedFifoState<T>);

impl<T> Future for FifoClosed<T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut guard = self.get_mut().0.0.lock().unwrap();
        if guard.capacity_bytes == 0 {
            return Poll::Ready(());
        }
        guard.waits_is_closed.register(cx.waker());
        Poll::Pending
    }
}

struct FifoState<T> {
    items: VecDeque<WithSize<T>>,
    size_bytes: usize,
    capacity_bytes: usize,
    waits_can_send: WakerSlot,
    waits_can_recv: WakerSlot,
    waits_is_closed: WakerSlot,
}

impl<T> FifoState<T> {
    fn try_push(
        &mut self,
        item: WithSize<T>,
        waker: &Waker,
    ) -> Result<Option<WithSize<T>>, FifoClosedError> {
        if self.capacity_bytes == 0 {
            return Err(FifoClosedError);
        }

        if self.items.is_empty().not() && (self.size_bytes + item.size > self.capacity_bytes) {
            self.waits_can_send.register(waker);
            return Ok(Some(item));
        }

        self.size_bytes += item.size;
        self.items.push_back(item);
        self.waits_can_recv.wake();

        Ok(None)
    }

    fn try_pop(&mut self, waker: &Waker) -> Result<Option<T>, FifoClosedError> {
        match self.items.pop_front() {
            Some(item) => {
                self.size_bytes -= item.size;
                self.waits_can_send.wake();
                self.items.smart_shrink();
                Ok(Some(item.item))
            }
            None if self.capacity_bytes == 0 => Err(FifoClosedError),
            None => {
                self.waits_can_recv.register(waker);
                Ok(None)
            }
        }
    }

    fn close(&mut self) {
        self.capacity_bytes = 0;
        if self.items.is_empty() {
            self.items = Default::default();
        }
        self.waits_is_closed.wake();
        self.waits_can_send.wake();
        self.waits_can_recv.wake();
    }
}

struct SharedFifoState<T>(Arc<Mutex<FifoState<T>>>);

impl<T> Clone for SharedFifoState<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> fmt::Debug for SharedFifoState<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("SharedFifoState");
        let ptr = Arc::as_ptr(&self.0);
        s.field("ptr", &format_args!("{ptr:p}"));
        match self.0.lock() {
            Ok(state) => {
                s.field("items_len", &state.items.len())
                    .field("items_capacity", &state.items.capacity())
                    .field("size_bytes", &state.size_bytes)
                    .field("capacity_bytes", &state.capacity_bytes);
            }
            Err(..) => {
                s.field("state", &"poisoned");
            }
        }
        s.finish()
    }
}

#[derive(Debug)]
struct WithSize<T> {
    item: T,
    size: usize,
}

/// Slot for a single [`Waker`].
#[derive(Default)]
struct WakerSlot(Option<Waker>);

impl WakerSlot {
    /// If this slot contains a waker, consumes and wakes it.
    fn wake(&mut self) {
        if let Some(waker) = self.0.take() {
            waker.wake();
        }
    }

    /// Registers a new waker, possibly replacing the current one.
    fn register(&mut self, waker: &Waker) {
        match &mut self.0 {
            Some(current) if current.will_wake(waker) => {}
            Some(current) => current.clone_from(waker),
            None => self.0 = Some(waker.clone()),
        }
    }
}

impl fmt::Debug for WakerSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_ref() {
            Some(..) => f.write_str("Filled"),
            None => f.write_str("Empty"),
        }
    }
}

#[cfg(test)]
mod test {
    use std::{num::NonZeroUsize, ops::Not, sync::Arc, time::Duration};

    use bytes::Bytes;
    use futures::{FutureExt, SinkExt, StreamExt};
    use rstest::rstest;
    use tokio::sync::Notify;

    use crate::fifo::Fifo;

    #[rstest]
    #[tokio::test]
    async fn closed_fut_works(#[values(true, false)] drop_sink: bool) {
        let Fifo {
            sink,
            stream,
            closed,
        } = Fifo::<Bytes>::with_capacity(NonZeroUsize::MIN);
        let closed_handle = tokio::spawn(closed);
        tokio::time::sleep(Duration::from_millis(30)).await;
        if drop_sink {
            drop(sink);
        } else {
            drop(stream);
        }
        closed_handle.await.unwrap();
    }

    #[tokio::test]
    async fn capacity_limit_works() {
        let Fifo {
            mut sink,
            mut stream,
            ..
        } = Fifo::<Bytes>::with_capacity(NonZeroUsize::new(512).unwrap());
        const ITEM: Bytes = Bytes::from_static(b"hello there general Kenobi");

        loop {
            let had_capacity = sink
                .send(ITEM.clone())
                .now_or_never()
                .transpose()
                .unwrap()
                .is_some();
            if had_capacity.not() {
                assert!(sink.state.0.lock().unwrap().capacity_bytes <= 512);
                sink.take_pending().unwrap();
                break;
            }
        }

        let _ = stream.next().await.unwrap();
        sink.send(ITEM.clone()).now_or_never().unwrap().unwrap();
    }

    #[rstest]
    #[tokio::test]
    async fn wakeups_work(#[values(true, false)] slow_consumer: bool) {
        let Fifo {
            mut sink,
            mut stream,
            ..
        } = Fifo::<Bytes>::with_capacity(NonZeroUsize::MIN);
        const ITEM: Bytes = Bytes::from_static(b"hello there general Kenobi");
        const COUNT: usize = 10;

        let notify = Arc::new(Notify::new());
        let start_consumer = notify.clone().notified_owned();
        let start_producer = notify.clone().notified_owned();

        let consumer = tokio::spawn(async move {
            start_consumer.await;
            for _ in 0..COUNT {
                if slow_consumer {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                stream.next().await.unwrap();
            }
        });
        let producer = tokio::spawn(async move {
            start_producer.await;
            for _ in 0..COUNT {
                if slow_consumer.not() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                sink.send(ITEM.clone()).await.unwrap();
            }
        });

        notify.notify_waiters();
        tokio::try_join!(producer, consumer).unwrap();
    }
}
