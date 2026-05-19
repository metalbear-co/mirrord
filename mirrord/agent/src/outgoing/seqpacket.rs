use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::{future::BoxFuture, stream::FuturesUnordered};
use mirrord_protocol::{
    ConnectionId, DaemonMessage, RemoteResult, outgoing::tcp::LayerSeqpacket, uid::Uid,
};
use streammap_ext::StreamMap;
use tokio::{
    io::WriteHalf,
    sync::{
        Semaphore,
        mpsc::{self, Receiver, Sender},
    },
};
use tracing::Level;

use crate::{
    error::AgentResult,
    metrics::SEQPACKET_CONNECTION,
    outgoing::{FuturesQueue, Throttled},
    task::{BgTaskRuntime, status::BgTaskStatus},
};

pub(crate) struct SeqpacketApi {
    task_status: BgTaskStatus,

    layer_tx: Sender<LayerSeqpacket>,

    daemon_rx: Receiver<Throttled<DaemonMessage>>,
}

impl SeqpacketApi {
    pub(crate) fn new(runtime: &BgTaskRuntime, pid: Option<u64>) -> Self {
        // IMPORTANT: this makes tokio tasks spawn on `runtime`.
        // Do not remove this.
        let _rt = runtime.handle().enter();

        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task_status = tokio::spawn(SeqpacketTask::new(pid, layer_rx, daemon_tx).run())
            .into_status("SeqpacketTask");

        Self {
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn send_to_task(&mut self, message: LayerSeqpacket) -> AgentResult<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }

    /// Receives a [`DaemonTcpOutgoing`] message from the background task.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) async fn recv_from_task(&mut self) -> AgentResult<Throttled<DaemonMessage>> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.wait_assert_running().await),
        }
    }
}

struct SeqpacketTask {}

impl Drop for SeqpacketTask {
    fn drop(&mut self) {
        let connections = self.readers.keys().chain(self.writers.keys()).count();
        SEQPACKET_CONNECTION.fetch_sub(connections, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SeqpacketTask {
    /// Buffer size for reading from the outgoing connections.
    const READ_BUFFER_SIZE: usize = 64 * 1024;
    /// How much incoming data we can accumulate in memory, before it's flushed to the client.
    ///
    /// This **must** be larger than [`Self::READ_BUFFER_SIZE`].
    const THROTTLE_PERMITS: usize = 512 * 1024;

    /// Timeout for connect attempts.
    ///
    /// # TODO(alex)
    /// This timeout works around the issue where golang tries to connect
    /// to an invalid socket address and hangs until the socket times out.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

    fn new(
        pid: Option<u64>,
        layer_rx: Receiver<LayerSeqpacket>,
        daemon_tx: Sender<Throttled<DaemonMessage>>,
    ) -> Self {
        Self {
            next_connection_id: 0,
            writers: Default::default(),
            readers: Default::default(),
            pid,
            layer_rx,
            daemon_tx,
            connects_v1: Default::default(),
            connects_v2: Default::default(),
            throttler: Arc::new(Semaphore::new(Self::THROTTLE_PERMITS)),
        }
    }
}
