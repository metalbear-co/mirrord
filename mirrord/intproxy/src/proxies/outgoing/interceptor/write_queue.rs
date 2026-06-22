use std::{ops::Not, time::Duration};

use mirrord_protocol::ClientMessage;
use mirrord_protocol_io::{Client, TxHandle};
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};

use crate::proxies::outgoing::{InterceptorId, OutgoingProxy};

struct QueuedAgentMessage {
    message: ClientMessage,
    delay: Duration,
}

pub(in crate::proxies::outgoing) struct AgentWriteQueue {
    tx: mpsc::Sender<QueuedAgentMessage>,
    handle: JoinHandle<()>,
}

impl AgentWriteQueue {
    pub(in crate::proxies::outgoing) fn new(
        id: InterceptorId,
        agent_tx: TxHandle<Client>,
        channel_size: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedAgentMessage>(channel_size);

        let handle = tokio::spawn(async move {
            while let Some(QueuedAgentMessage { message, delay }) = rx.recv().await {
                if delay.is_zero().not() {
                    tracing::trace!(%id, ?delay, "Delaying outgoing message to agent");
                    sleep(delay).await;
                }

                agent_tx.send(message).await;
            }
        });

        Self { tx, handle }
    }

    pub(in crate::proxies::outgoing) async fn send(&self, message: ClientMessage, delay: Duration) {
        let _ = self.tx.send(QueuedAgentMessage { message, delay }).await;
    }

    pub(in crate::proxies::outgoing) fn abort(self) {
        self.handle.abort();
    }
}

impl OutgoingProxy {
    pub(crate) async fn queue_agent_message(
        &mut self,
        id: InterceptorId,
        message: ClientMessage,
        delay: Duration,
    ) {
        if let Some(queue) = self.agent_write_queues.get(&id) {
            queue.send(message, delay).await;
        }
    }

    pub(crate) async fn finish_agent_write_queue(
        &mut self,
        id: InterceptorId,
        message: ClientMessage,
        delay: Duration,
    ) {
        if let Some(queue) = self.agent_write_queues.remove(&id) {
            queue.send(message, delay).await;
        }
    }

    pub(crate) fn abort_agent_write_queue(&mut self, id: &InterceptorId) {
        if let Some(queue) = self.agent_write_queues.remove(id) {
            queue.abort();
        }
    }

    pub(crate) fn abort_all_agent_write_queues(&mut self) {
        for queue in std::mem::take(&mut self.agent_write_queues).into_values() {
            queue.abort();
        }
    }
}
