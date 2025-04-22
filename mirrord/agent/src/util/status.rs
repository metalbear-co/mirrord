use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::Shared, FutureExt};
use thiserror::Error;
use tokio::sync::oneshot;

#[derive(Error, Debug, Clone, Copy)]
#[error("status channel was dropped")]
pub struct StatusTxDropped;

#[derive(Error, Debug, Clone, Copy)]
#[error("task panicked")]
pub struct Panicked;

pub trait Status: Clone + Sized + fmt::Debug {
    fn panicked() -> Self;

    fn dropped() -> Self;
}

impl<T: Clone + fmt::Debug + From<Panicked> + From<StatusTxDropped>> Status for T {
    fn panicked() -> Self {
        Panicked.into()
    }

    fn dropped() -> Self {
        StatusTxDropped.into()
    }
}

impl<T: Clone + fmt::Debug, E: Status> Status for Result<T, E> {
    fn panicked() -> Self {
        Err(E::panicked())
    }

    fn dropped() -> Self {
        Err(E::dropped())
    }
}

#[derive(Clone)]
pub struct StatusReceiver<S: Status>(Shared<oneshot::Receiver<S>>);

impl<S: Status> Future for StatusReceiver<S> {
    type Output = S;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0
            .clone()
            .poll_unpin(cx)
            .map(|result| result.expect("StatusSender sends a value on drop"))
    }
}

impl<S: Status> fmt::Debug for StatusReceiver<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.clone().now_or_never().fmt(f)
    }
}

pub struct StatusSender<S: Status>(Option<oneshot::Sender<S>>);

impl<S: Status> StatusSender<S> {
    pub fn send(mut self, status: S) {
        let _ = self
            .0
            .take()
            .expect("channel is not consumed while the struct exists")
            .send(status);
    }
}

impl<S: Status> Drop for StatusSender<S> {
    fn drop(&mut self) {
        let Some(tx) = self.0.take() else {
            return;
        };

        let status = if std::thread::panicking() {
            S::panicked()
        } else {
            S::dropped()
        };

        let _ = tx.send(status);
    }
}

pub fn channel<S: Status>() -> (StatusSender<S>, StatusReceiver<S>) {
    let (tx, rx) = oneshot::channel();

    (StatusSender(Some(tx)), StatusReceiver(rx.shared()))
}
