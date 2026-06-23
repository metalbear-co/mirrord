use std::{ops::Not, time::Duration};

use bytes::Bytes;
use tokio::{sync::mpsc, time::sleep};
use tokio_util::task::AbortOnDropHandle;

use super::Interceptor;
use crate::{
    background_tasks::TaskSender,
    proxies::outgoing::{InterceptorId, OutgoingProxy},
};

/// The [`Bytes`] that we're reading from the intercepted connection, with a delay before we send
/// them to the layer that asked for it.
struct QueuedInterceptorMessage {
    /// [`Bytes`] that can be read from this intercepted connection.
    bytes: Bytes,

    /// How long to `sleep` for before sending the [`Self::bytes`].
    delay: Duration,
}

/// Holds the [`mpsc::Sender`] that we use to send [`QueuedInterceptorMessage`]s to the associated
/// task.
///
/// When a `ChaosSelector` matches some outgoing traffic, then we handle the bytes of read
/// operations for this intercepted connection in the task with [`Self::handle`]. We have to do this
/// to prevent blocking the main [`OutgoingProxy`] loop, and to keep the messages being sent in
/// order.
pub(crate) struct InterceptorReadQueue {
    /// [`mpsc::Sender`] for the [`QueuedInterceptorMessage`]s that are handled by the task.
    tx: mpsc::Sender<QueuedInterceptorMessage>,

    /// Task that keeps calling [`mpsc::Receiver::recv`] on the receiver side of [`Self::tx`],
    /// getting the [`QueuedInterceptorMessage`]s that should be sent to the agent, after maybe
    /// sleeping according to the `ChaosRule`.
    handle: AbortOnDropHandle<()>,
}

impl InterceptorReadQueue {
    /// Creates the [`InterceptorReadQueue`] and starts the receiving task, see
    /// [`InterceptorReadQueue::handle`].
    pub(crate) fn new(interceptor: TaskSender<Interceptor>) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedInterceptorMessage>(OutgoingProxy::CHANNEL_SIZE);

        let handle = tokio::spawn(async move {
            while let Some(QueuedInterceptorMessage { bytes, delay }) = rx.recv().await {
                if delay.is_zero().not() {
                    sleep(delay).await;
                }

                interceptor.send(bytes).await;
            }
        });

        Self {
            tx,
            handle: AbortOnDropHandle::new(handle),
        }
    }

    /// Helper to send the `message` with `delay` on [`Self::tx`].
    async fn send(&self, bytes: Bytes, delay: Duration) {
        let _ = self
            .tx
            .send(QueuedInterceptorMessage { bytes, delay })
            .await;
    }

    /// Sends the last `bytes` through [`Self::tx`] to the background task.
    ///
    /// We [`AbortOnDropHandle::detach`] here to give the task enough time to process this message,
    /// afterwards it'll try to read another message, see that the channel is closed (we drop `tx`
    /// here) and this will end the task.
    async fn finish(self, bytes: Bytes, delay: Duration) {
        let Self { tx, handle } = self;

        let _ = tx.send(QueuedInterceptorMessage { bytes, delay }).await;
        let _ = handle.detach();
    }
}

impl OutgoingProxy {
    /// Sends the `bytes` on the [`InterceptorReadQueue::tx`], if the `id` is of one of the
    /// [`InterceptorId`]s that we're handling (some `ChaosSelector` hit this outgoing connection).
    pub(crate) async fn queue_interceptor_message(
        &mut self,
        id: InterceptorId,
        bytes: Bytes,
        delay: Duration,
    ) -> bool {
        if let Some(queue) = self.interceptor_read_queues.get(&id) {
            queue.send(bytes, delay).await;
            true
        } else {
            false
        }
    }

    /// We received a `Daemon***:::Close` message, indicating that this connection should go kaput,
    /// so we remove the task with [`InterceptorId`], and call [`InterceptorReadQueue::finish`] on
    /// it.
    pub(crate) async fn finish_interceptor_read_queue(
        &mut self,
        id: InterceptorId,
        bytes: Bytes,
        delay: Duration,
    ) {
        if let Some(queue) = self.interceptor_read_queues.remove(&id) {
            queue.finish(bytes, delay).await;
        }
    }

    /// We received a `***Close` message from the agent, so we remove the [`InterceptorReadQueue`]
    /// for this [`InterceptorId`], effectively aborting the queue task.
    ///
    /// The difference from the [`Self::finish_interceptor_read_queue`] is that here, the
    /// interceptor task has been finished.
    pub(crate) fn abort_interceptor_read_queue(&mut self, id: &InterceptorId) -> bool {
        self.interceptor_read_queues.remove(id).is_some()
    }

    /// Similar to [`Self::abort_interceptor_read_queue`], except here we're dealing with a
    /// `ConnectionRefresh::Start` message, so we drop everything and start anew.
    pub(crate) fn abort_all_interceptor_read_queues(&mut self) {
        self.interceptor_read_queues.clear();
    }
}
