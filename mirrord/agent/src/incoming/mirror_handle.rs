use std::fmt;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{wrappers::ReceiverStream, StreamMap, StreamNotifyClose};

use super::{
    connection::{http::MirroredHttp, tcp::MirroredTcp},
    task::{RedirectionRequest, TaskError},
    RedirectorTaskError,
};

/// Handle to a running [`RedirectorTask`](super::task::RedirectorTask).
///
/// Allows for mirroring incoming traffic.
///
/// # Clone implementation
///
/// This handle can be cloned and used by multiple agent clients.
/// A new handle acquired with [`Clone::clone`] is independent and does not inherit port
/// subscriptions of its parent.
pub struct MirrorHandle {
    /// For sending mirror requests to the task.
    message_tx: mpsc::Sender<RedirectionRequest>,
    /// For fetching the task error.
    ///
    /// [`RedirectorTask`](super::RedirectorTask) never exits before all handles is dropped.
    /// Also, it never removes any port mirror on its own.
    /// Therefore, if one of our [`mpsc::channel`]s fails, we assume that the task has failed
    /// and we use this channel to retrieve the task's error.
    task_error: TaskError,
    /// For receiving mirrored traffic.
    mirrored_ports: StreamMap<u16, StreamNotifyClose<ReceiverStream<MirroredTraffic>>>,
}

impl MirrorHandle {
    pub(super) fn new(message_tx: mpsc::Sender<RedirectionRequest>, task_error: TaskError) -> Self {
        Self {
            message_tx,
            task_error,
            mirrored_ports: Default::default(),
        }
    }

    /// Issues a request to start mirroring from the given port.
    ///
    /// If this port is already mirrored, does nothing.
    ///
    /// If this method returns [`Ok`], it means that the port redirection
    /// was done in the [`RedirectorTask`](super::RedirectorTask),
    /// and incoming connections are now being mirrored.
    pub async fn mirror(&mut self, port: u16) -> Result<(), RedirectorTaskError> {
        if self.mirrored_ports.contains_key(&port) {
            return Ok(());
        };

        let (receiver_tx, receiver_rx) = oneshot::channel();
        if self
            .message_tx
            .send(RedirectionRequest::Mirror { port, receiver_tx })
            .await
            .is_err()
        {
            return Err(self.task_error.get().await);
        }

        let Ok(rx) = receiver_rx.await else {
            return Err(self.task_error.get().await);
        };

        self.mirrored_ports
            .insert(port, StreamNotifyClose::new(ReceiverStream::new(rx)));

        Ok(())
    }

    /// Stops mirroring the given port.
    ///
    /// If this port is not mirorred, does nothing.
    pub fn stop_mirror(&mut self, port: u16) {
        self.mirrored_ports.remove(&port);
    }

    /// Returns mirrored traffic.
    ///
    /// Returns nothing if no port is mirrored.
    pub async fn next(&mut self) -> Option<Result<MirroredTraffic, RedirectorTaskError>> {
        match self.mirrored_ports.next().await? {
            (.., Some(conn)) => Some(Ok(conn)),
            (.., None) => Some(Err(self.task_error.get().await)),
        }
    }
}

impl Clone for MirrorHandle {
    fn clone(&self) -> Self {
        Self {
            message_tx: self.message_tx.clone(),
            task_error: self.task_error.clone(),
            mirrored_ports: Default::default(),
        }
    }
}

/// Incoming traffic mirrored with a [`MirrorHandle`].
pub enum MirroredTraffic {
    Tcp(MirroredTcp),
    Http(MirroredHttp),
}

#[cfg(test)]
impl MirroredTraffic {
    pub fn unwrap_tcp(self) -> MirroredTcp {
        match self {
            MirroredTraffic::Tcp(tcp) => tcp,
            MirroredTraffic::Http(..) => panic!("expected TCP, got HTTP"),
        }
    }
}

impl fmt::Debug for MirrorHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirrorHandle")
            .field("task_error", &self.task_error)
            .field("channel_closed", &self.message_tx.is_closed())
            .field(
                "queued_messages",
                &(self.message_tx.max_capacity() - self.message_tx.capacity()),
            )
            .field("mirrored_ports", &self.mirrored_ports.len())
            .finish()
    }
}
