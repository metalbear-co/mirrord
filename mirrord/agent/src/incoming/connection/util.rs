use tokio::sync::mpsc;

use crate::incoming::error::StealerDropped;

/// Wrapper over an [`mpsc::Sender`].
///
/// [`StealerSender::send`] returns a meaningful error.
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
