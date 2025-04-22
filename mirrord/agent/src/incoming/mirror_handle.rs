use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{wrappers::ReceiverStream, StreamMap, StreamNotifyClose};

use super::{
    connection::{http::MirroredHttp, tcp::MirroredTcp},
    task::RedirectionRequest,
    RedirectorTaskError,
};
use crate::util::status::StatusReceiver;

pub struct MirrorHandle {
    /// For sending mirror requests to the task.
    message_tx: mpsc::Sender<RedirectionRequest>,
    /// For fetching the task error.
    ///
    /// [`RedirectorTask`](super::RedirectorTask) never exits before this handle is dropped.
    /// Also, it never removes any port mirror on its own.
    /// Therefore, if one of our [`mpsc::channel`]s fails, we assume that the task has failed
    /// and we use this channel to retrieve the task's error.
    task_status: StatusReceiver<RedirectorTaskError>,
    /// For receiving mirrored traffic.
    mirrored_ports: StreamMap<u16, StreamNotifyClose<ReceiverStream<MirroredTraffic>>>,
}

impl MirrorHandle {
    pub(super) fn new(
        message_tx: mpsc::Sender<RedirectionRequest>,
        task_status: StatusReceiver<RedirectorTaskError>,
    ) -> Self {
        Self {
            message_tx,
            task_status,
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
            return Err(self.task_status.clone().await);
        }

        let Ok(rx) = receiver_rx.await else {
            return Err(self.task_status.clone().await);
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
            (.., None) => Some(Err(self.task_status.clone().await)),
        }
    }
}

impl Clone for MirrorHandle {
    fn clone(&self) -> Self {
        Self {
            message_tx: self.message_tx.clone(),
            task_status: self.task_status.clone(),
            mirrored_ports: Default::default(),
        }
    }
}

pub enum MirroredTraffic {
    Tcp(MirroredTcp),
    Http(MirroredHttp),
}
