use std::{
    io,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::Sink;
use pin_project_lite::pin_project;
use tokio::time::{Instant, Sleep};

pin_project! {
    /// [`Sink`] that applies a timeout to all operations.
    ///
    /// Should be used when proxying data from the client.
    /// This is a remedy for the fact that mirrord-protocol has no flow control.
    ///
    /// # Timeout logic
    ///
    /// Timeout count starts when any of the [`Sink`] methods returns [`Poll::Pending`].
    /// Timeout is disarmed when any of the [`Sink`] methods returns [`Poll::Ready`],
    /// or [`Sink::start_send`] succeeds.
    pub struct TimeoutSink<S> {
        #[pin]
        sleep: Sleep,
        #[pin]
        sink: S,
        timeout: Duration,
        armed: bool,
    }
}

impl<S> TimeoutSink<S> {
    pub fn new(sink: S, timeout: Duration) -> Self {
        Self {
            sleep: tokio::time::sleep(Duration::ZERO),
            sink,
            timeout,
            armed: false,
        }
    }
}

impl<T, S: Sink<T, Error = io::Error>> Sink<T> for TimeoutSink<S> {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        if this.sink.poll_ready(cx)?.is_pending() {
            if this.armed.not() {
                *this.armed = true;
                this.sleep.as_mut().reset(Instant::now() + *this.timeout);
            }
            std::task::ready!(this.sleep.poll(cx));
            Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
        } else {
            *this.armed = false;
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        let this = self.project();
        this.sink.start_send(item)?;
        *this.armed = false;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        if this.sink.poll_flush(cx)?.is_pending() {
            if this.armed.not() {
                *this.armed = true;
                this.sleep.as_mut().reset(Instant::now() + *this.timeout);
            }
            std::task::ready!(this.sleep.poll(cx));
            Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
        } else {
            *this.armed = false;
            Poll::Ready(Ok(()))
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        if this.sink.poll_close(cx)?.is_pending() {
            if this.armed.not() {
                *this.armed = true;
                this.sleep.as_mut().reset(Instant::now() + *this.timeout);
            }
            std::task::ready!(this.sleep.poll(cx));
            Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
        } else {
            *this.armed = false;
            Poll::Ready(Ok(()))
        }
    }
}
