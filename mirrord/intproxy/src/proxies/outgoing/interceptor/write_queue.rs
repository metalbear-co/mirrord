use std::time::Duration;

use mirrord_protocol::ClientMessage;
use mirrord_protocol_io::{Client, TxHandle};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc},
    time::Instant,
};
use tokio_stream::StreamExt;
use tokio_util::task::AbortOnDropHandle;

use super::delay_queue::DelayQueue;
use crate::proxies::outgoing::{InterceptorId, OutgoingProxy};

/// The [`ClientMessage`] that we want to send to the agent at [`Self::deadline`].
///
/// We send these through the [`AgentWriteQueue`] if an outgoing connection should be handled by
/// some `ChaosRule`.
struct QueuedAgentMessage {
    /// Regular [`ClientMessage`] to be sent to the agent, at [`Self::deadline`].
    message: ClientMessage,

    /// When this message becomes due to be sent to the agent.
    /// Computed as `now + delay` at enqueue time.
    deadline: Instant,

    /// Write-budget permit covering [`Self::message`]'s payload, released on drop after the
    /// message is sent. [`None`] for messages that do not consume budget.
    permit: Option<OwnedSemaphorePermit>,
}

/// Holds the [`mpsc::Sender`] that we use to send [`QueuedAgentMessage`]s to the associated task.
///
/// When a `ChaosSelector` matches some outgoing traffic, then we handle the [`ClientMessage`]s for
/// this intercepted connection in the task spawned by [`Self::new`]. We have to do this to prevent
/// blocking the main [`OutgoingProxy`] loop, and to keep the messages being sent in order.
pub(crate) struct AgentWriteQueue {
    /// [`mpsc::Sender`] for the [`QueuedAgentMessage`]s that are handled by the task.
    tx: mpsc::Sender<QueuedAgentMessage>,

    /// Task that moves [`QueuedAgentMessage`]s from [`Self::tx`] into a [`DelayQueue`] and sends
    /// each one to the agent once its [`QueuedAgentMessage::deadline`] is reached.
    handle: AbortOnDropHandle<()>,
}

impl AgentWriteQueue {
    /// Creates the [`AgentWriteQueue`] and starts the background task, see
    /// [`AgentWriteQueue::handle`].
    pub(crate) fn new(agent_tx: TxHandle<Client>) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedAgentMessage>(OutgoingProxy::CHANNEL_SIZE);

        let handle = tokio::spawn(async move {
            let mut queue = DelayQueue::<(ClientMessage, Option<OwnedSemaphorePermit>)>::default();

            loop {
                tokio::select! {
                    queued = rx.recv() => match queued {
                        Some(QueuedAgentMessage { message, deadline, permit }) => {
                            queue.push((message, permit), deadline);
                        }
                        // OutgoingProxy dropped the sender.
                        None => break,
                    },

                    // `None` does NOT mean end of stream.
                    // Guarded with is_empty so an empty queue doesn't cause a busy loop.
                    Some((message, permit)) = queue.next(), if !queue.is_empty() => {
                        agent_tx.send(message).await;
                        // Explicitly dropping the permit to release client-to-agent write budget.
                        drop(permit);
                    }
                }
            }

            // Flush whatever is still queued (the final close sent by `finish`, plus any data
            // enqueued before it), honoring each message's deadline, before ending.
            while let Some((message, permit)) = queue.next().await {
                agent_tx.send(message).await;
                drop(permit);
            }
        });

        Self {
            tx,
            handle: AbortOnDropHandle::new(handle),
        }
    }

    /// Helper to enqueue `message` with a release deadline and the write-budget permit covering
    /// the message payload.
    async fn send(
        &self,
        message: ClientMessage,
        delay: Duration,
        permit: Option<OwnedSemaphorePermit>,
    ) {
        let _ = self
            .tx
            .send(QueuedAgentMessage {
                message,
                deadline: Instant::now() + delay,
                permit,
            })
            .await;
    }

    /// Sends one last `message` through [`Self::tx`] to the background task.
    ///
    /// This is the proxy-generated connection close, which did not consume write budget, so it
    /// carries no permit.
    ///
    /// We [`AbortOnDropHandle::detach`] here to give the task enough time to process this message,
    /// afterwards it'll try to read another message, see that the channel is closed (we drop `tx`
    /// here) and this will end the task.
    async fn finish(self, message: ClientMessage, delay: Duration) {
        let Self { tx, handle } = self;

        let _ = tx
            .send(QueuedAgentMessage {
                message,
                deadline: Instant::now() + delay,
                permit: None,
            })
            .await;
        drop(handle.detach());
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
        permit: Option<OwnedSemaphorePermit>,
    ) {
        if let Some(queue) = self.agent_write_queues.get(&id) {
            queue.send(message, delay, permit).await;
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
