use std::fmt;

use futures::StreamExt;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{StreamMap, StreamNotifyClose, wrappers::ReceiverStream};
use tokio_util::sync::CancellationToken;

use super::{
    connection::{ConnectionInfo, http::RedirectedHttp, tcp::RedirectedTcp},
    error::RedirectorTaskError,
    task::{RedirectRequest, TaskError},
};

/// Handle to a running [`RedirectorTask`](super::task::RedirectorTask).
///
/// Allows for stealing incoming TCP connections.
pub struct StealHandle {
    /// For sending steal requests to the task.
    message_tx: mpsc::Sender<RedirectRequest>,
    /// For fetching the task error.
    ///
    /// [`RedirectorTask`](super::RedirectorTask) never exits before this handle is dropped.
    /// Also, it never removes any port steal on its own.
    /// Therefore, if one of our [`mpsc::channel`]s fails, we assume that the task has failed
    /// and we use this [`TaskError`] to retrieve the task's error.
    task_error: TaskError,
    /// For receiving stolen connections.
    stolen_ports: StreamMap<u16, StreamNotifyClose<ReceiverStream<StolenTraffic>>>,
}

impl StealHandle {
    pub(super) fn new(message_tx: mpsc::Sender<RedirectRequest>, task_error: TaskError) -> Self {
        Self {
            message_tx,
            task_error,
            stolen_ports: Default::default(),
        }
    }

    /// Issues a request to start stealing from the given port.
    ///
    /// If this port is already stolen, does nothing.
    ///
    /// If this method returns [`Ok`], it means that the port redirection
    /// was done in the [`RedirectorTask`](super::RedirectorTask),
    /// and incoming connections are now being stolen.
    pub async fn steal(&mut self, port: u16) -> Result<(), RedirectorTaskError> {
        if self.stolen_ports.contains_key(&port) {
            return Ok(());
        };

        let (receiver_tx, receiver_rx) = oneshot::channel();
        if self
            .message_tx
            .send(RedirectRequest::Steal { port, receiver_tx })
            .await
            .is_err()
        {
            return Err(self.task_error.get().await);
        }

        let Ok(rx) = receiver_rx.await else {
            return Err(self.task_error.get().await);
        };

        self.stolen_ports
            .insert(port, StreamNotifyClose::new(ReceiverStream::new(rx)));

        Ok(())
    }

    /// Stops stealing the given port.
    ///
    /// If this port is not stolen, does nothing.
    pub fn stop_steal(&mut self, port: u16) {
        // This drops our traffic `mpsc::Receiver`,
        // which should be detected by the `RedirectorTask`.
        self.stolen_ports.remove(&port);
    }

    /// Returns stolen traffic.
    ///
    /// Returns nothing if no port is stolen.
    pub async fn next(&mut self) -> Option<Result<StolenTraffic, RedirectorTaskError>> {
        match self.stolen_ports.next().await? {
            (.., Some(conn)) => Some(Ok(conn)),
            (.., None) => Some(Err(self.task_error.get().await)),
        }
    }
}

#[derive(Debug)]
pub enum StolenTraffic {
    Tcp {
        conn: RedirectedTcp,
        /// Used for returning the [`JoinHandle`] to the spawned IO
        /// task so [`RedirectorTask`] can join it before flushing
        /// iptables.
        join_handle_tx: oneshot::Sender<JoinHandle<()>>,
        /// Used for gracefully shutting down the IO task.
        shutdown: CancellationToken,
    },
    Http(RedirectedHttp),
}

impl StolenTraffic {
    pub fn info(&self) -> &ConnectionInfo {
        match self {
            StolenTraffic::Tcp { conn, .. } => conn.info(),
            StolenTraffic::Http(conn) => conn.info(),
        }
    }
}

impl fmt::Debug for StealHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealHandle")
            .field("task_error", &self.task_error)
            .field("channel_closed", &self.message_tx.is_closed())
            .field(
                "queued_messages",
                &(self.message_tx.max_capacity() - self.message_tx.capacity()),
            )
            .field("stolen_ports", &self.stolen_ports.len())
            .finish()
    }
}
