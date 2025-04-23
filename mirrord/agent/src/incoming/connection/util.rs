use tokio::sync::{broadcast, mpsc};

use crate::incoming::error::StealerDropped;

/// Wrapper over an optional [`broadcast::Sender`].
///
/// The inner sender is dropped as soon as we detect that the channel is closed.
/// This is to eagerly free the resources allocated for the channel.
#[derive(Clone)]
pub struct AutoDropBroadcast<T>(Option<broadcast::Sender<T>>);

impl<T: Clone> AutoDropBroadcast<T> {
    pub fn send<M: Into<T>>(&mut self, message: M) {
        let Some(tx) = &mut self.0 else {
            return;
        };

        if tx.send(message.into()).is_err() {
            self.0 = None;
        }
    }
}

impl<T> From<broadcast::Sender<T>> for AutoDropBroadcast<T> {
    fn from(value: broadcast::Sender<T>) -> Self {
        let value = (value.receiver_count() > 0).then_some(value);
        Self(value)
    }
}

impl<T> From<Option<broadcast::Sender<T>>> for AutoDropBroadcast<T> {
    fn from(value: Option<broadcast::Sender<T>>) -> Self {
        Self(value)
    }
}

/// Wrapper over an [`mpsc::Sender`].
///
/// [`StealerSender::send`] returns a meaningful error that implements [`Into<ConnError>`].
#[derive(Clone)]
pub struct StealerSender<T>(mpsc::Sender<T>);

impl<T> StealerSender<T> {
    pub async fn send(&self, message: T) -> Result<(), StealerDropped> {
        self.0.send(message).await.map_err(|_| StealerDropped)
    }
}

impl<T> From<mpsc::Sender<T>> for StealerSender<T> {
    fn from(value: mpsc::Sender<T>) -> Self {
        Self(value)
    }
}
