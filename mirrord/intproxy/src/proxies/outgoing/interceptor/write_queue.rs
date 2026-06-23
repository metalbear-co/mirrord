use std::{ops::Not, time::Duration};

use mirrord_protocol::ClientMessage;
use mirrord_protocol_io::{Client, TxHandle};
use tokio::{sync::mpsc, time::sleep};
use tokio_util::task::AbortOnDropHandle;

use crate::proxies::outgoing::{InterceptorId, OutgoingProxy};

/// The [`ClientMessage`] that we want to send with a potential `delay` to the agent.
///
/// We send these through the [`AgentWriteQueue`] if an outgoing connection should be handled by
/// some `ChaosRule`.
struct QueuedAgentMessage {
    /// Regular [`ClientMessage`] to be sent to the agent, at some point.
    message: ClientMessage,

    /// How long to `sleep` for before sending the [`Self::message`].
    delay: Duration,
}

/// Holds the [`mpsc::Sender`] that we use to send [`QueuedAgentMessage`]s to the associated task.
///
/// When a `ChaosSelector` matches some outgoing traffic, then we handle the [`ClientMessage`]s for
/// this intercepted connection in the task with [`Self::handle`]. We have to do this to prevent
/// blocking the main [`OutgoingProxy`] loop, and to keep the messages being sent in order.
pub(crate) struct AgentWriteQueue {
    /// [`mpsc::Sender`] for the [`QueuedAgentMessage`]s that are handled by the task.
    tx: mpsc::Sender<QueuedAgentMessage>,

    /// Task that keeps calling [`mpsc::Receiver::recv`] on the receiver side of [`Self::tx`],
    /// getting the [`QueuedAgentMessage`]s that should be sent to the agent, after maybe sleeping
    /// according to the `ChaosRule`.
    handle: AbortOnDropHandle<()>,
}

impl AgentWriteQueue {
    /// Creates the [`AgentWriteQueue`] and starts the receiving task, see
    /// [`AgentWriteQueue::handle`].
    pub(crate) fn new(agent_tx: TxHandle<Client>) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedAgentMessage>(OutgoingProxy::CHANNEL_SIZE);

        let handle = tokio::spawn(async move {
            while let Some(QueuedAgentMessage { message, delay }) = rx.recv().await {
                if delay.is_zero().not() {
                    sleep(delay).await;
                }

                agent_tx.send(message).await;
            }
        });

        Self {
            tx,
            handle: AbortOnDropHandle::new(handle),
        }
    }

    /// Helper to send the `message` with `delay` on [`Self::tx`].
    async fn send(&self, message: ClientMessage, delay: Duration) {
        let _ = self.tx.send(QueuedAgentMessage { message, delay }).await;
    }

    /// Sends one last `message` through [`Self::tx`] to the background task.
    ///
    /// We [`AbortOnDropHandle::detach`] here to give the task enough time to process this message,
    /// afterwards it'll try to read another message, see that the channel is closed (we drop `tx`
    /// here) and this will end the task.
    async fn finish(self, message: ClientMessage, delay: Duration) {
        let Self { tx, handle } = self;

        let _ = tx.send(QueuedAgentMessage { message, delay }).await;
        let _ = handle.detach();
    }
}

impl OutgoingProxy {
    /// Sends the `message` on the [`AgentWriteQueue::tx`], if the `id` is of one of the
    /// [`InterceptorId`]s that we're handling (some `ChaosSelector` hit this outgoing connection).
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

    /// The read side of this connection is finishing, so we send a `LayerClose` to the agent, and
    /// [`AgentWriteQueue::finish`] this write queue.
    pub(crate) async fn finish_agent_write_queue(
        &mut self,
        id: InterceptorId,
        message: ClientMessage,
        delay: Duration,
    ) {
        if let Some(queue) = self.agent_write_queues.remove(&id) {
            queue.finish(message, delay).await;
        }
    }

    /// We received a `***Close` message from the agent, so we remove the [`AgentWriteQueue`] for
    /// this [`InterceptorId`], effectively aborting the queue task.
    pub(crate) fn abort_agent_write_queue(&mut self, id: &InterceptorId) {
        self.agent_write_queues.remove(id);
    }

    /// Similar to [`Self::abort_agent_write_queue`], except here we're dealing with a
    /// `ConnectionRefresh::Start` message, so we drop everything and start anew.
    pub(crate) fn abort_all_agent_write_queues(&mut self) {
        self.agent_write_queues.clear();
    }
}
