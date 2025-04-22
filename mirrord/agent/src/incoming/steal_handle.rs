use std::fmt;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{wrappers::ReceiverStream, StreamMap, StreamNotifyClose};

use super::{
    connection::{http::RedirectedHttp, tcp::RedirectedTcp},
    error::RedirectorTaskError,
    task::RedirectionRequest,
};
use crate::util::status::StatusReceiver;

/// Handle to a running [`RedirectorTask`](super::task::RedirectorTask).
///
/// Allows for stealing incoming traffic.
pub struct StealHandle {
    /// For sending steal requests to the task.
    message_tx: mpsc::Sender<RedirectionRequest>,
    /// For fetching the task error.
    ///
    /// [`RedirectorTask`](super::RedirectorTask) never exits before this handle is dropped.
    /// Also, it never removes any port steal on its own.
    /// Therefore, if one of our [`mpsc::channel`]s fails, we assume that the task has failed
    /// and we use this channel to retrieve the task's error.
    task_status: StatusReceiver<RedirectorTaskError>,
    /// For receiving stolen traffic.
    stolen_ports: StreamMap<u16, StreamNotifyClose<ReceiverStream<StolenTraffic>>>,
}

impl StealHandle {
    pub(super) fn new(
        message_tx: mpsc::Sender<RedirectionRequest>,
        task_status: StatusReceiver<RedirectorTaskError>,
    ) -> Self {
        Self {
            message_tx,
            task_status,
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
            .send(RedirectionRequest::Steal { port, receiver_tx })
            .await
            .is_err()
        {
            return Err(self.task_status.clone().await);
        }

        let Ok(rx) = receiver_rx.await else {
            return Err(self.task_status.clone().await);
        };

        self.stolen_ports
            .insert(port, StreamNotifyClose::new(ReceiverStream::new(rx)));

        Ok(())
    }

    /// Stops stealing the given port.
    ///
    /// If this port is not stolen, does nothing.
    pub fn stop_steal(&mut self, port: u16) {
        self.stolen_ports.remove(&port);
    }

    /// Returns stolen traffic.
    ///
    /// Returns nothing if no port is stolen.
    pub async fn next(&mut self) -> Option<Result<StolenTraffic, RedirectorTaskError>> {
        match self.stolen_ports.next().await? {
            (.., Some(conn)) => Some(Ok(conn)),
            (.., None) => Some(Err(self.task_status.clone().await)),
        }
    }
}

pub enum StolenTraffic {
    Tcp(RedirectedTcp),
    Http(RedirectedHttp),
}

impl StolenTraffic {
    pub fn destination_port(&self) -> u16 {
        match self {
            Self::Tcp(tcp) => tcp.info().original_destination.port(),
            Self::Http(http) => http.info().original_destination.port(),
        }
    }
}

impl fmt::Debug for StealHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealHandle")
            .field("task_status", &self.task_status)
            .field("channel_closed", &self.message_tx.is_closed())
            .field(
                "queued_messages",
                &(self.message_tx.max_capacity() - self.message_tx.capacity()),
            )
            .field("stolen_ports", &self.stolen_ports.len())
            .finish()
    }
}
