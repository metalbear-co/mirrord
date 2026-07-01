use std::time::Duration;

use bytes::Bytes;
use tokio::{sync::mpsc, time::Instant};
use tokio_stream::StreamExt;
use tokio_util::task::AbortOnDropHandle;

use super::{Interceptor, InterceptorCommand, delay_queue::DelayQueue};
use crate::{
    background_tasks::TaskSender,
    proxies::outgoing::{InterceptorId, OutgoingProxy},
};

/// The [`InterceptorCommand`] that we're sending to the intercepted connection at
/// [`Self::deadline`].
struct QueuedInterceptorMessage {
    /// Command to run on this intercepted connection.
    command: InterceptorCommand,

    /// When these [`Self::command`] ([`InterceptorCommand::Data`]) become due to be sent to the
    /// layer. Computed as `now + delay` at enqueue time.
    deadline: Instant,
}

/// Holds the [`mpsc::Sender`] that we use to send [`QueuedInterceptorMessage`]s to the associated
/// task.
///
/// When a `ChaosSelector` matches some outgoing traffic, then we handle the bytes of read
/// operations for this intercepted connection in the task spawned by [`Self::new`]. We have to do
/// this to prevent blocking the main [`OutgoingProxy`] loop, and to keep the messages being sent in
/// order.
pub(crate) struct InterceptorReadQueue {
    /// [`mpsc::Sender`] for the [`QueuedInterceptorMessage`]s that are handled by the task.
    tx: mpsc::Sender<QueuedInterceptorMessage>,

    /// Task that moves [`QueuedInterceptorMessage`]s from [`Self::tx`] into a [`DelayQueue`] and
    /// sends each one to the [`Interceptor`] once its [`QueuedInterceptorMessage::deadline`] is
    /// reached.
    handle: AbortOnDropHandle<()>,
}

impl InterceptorReadQueue {
    /// Creates the [`InterceptorReadQueue`] and starts the background task, see
    /// [`InterceptorReadQueue::handle`].
    pub(crate) fn new(interceptor: TaskSender<Interceptor>) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedInterceptorMessage>(OutgoingProxy::CHANNEL_SIZE);

        let handle = tokio::spawn(async move {
            let mut queue = DelayQueue::<InterceptorCommand>::default();

            loop {
                tokio::select! {
                    queued = rx.recv() => match queued {
                        Some(QueuedInterceptorMessage { command, deadline }) => {
                            queue.push(command, deadline);
                        }
                        // OutgoingProxy dropped the sender.
                        None => break,
                    },

                    // `None` does NOT mean end of stream.
                    // Guarded with is_empty so an empty queue doesn't cause a busy loop.
                    Some(command) = queue.next(), if !queue.is_empty() => {
                        interceptor.send(command).await;
                    }
                }
            }

            // Flush whatever is still queued (the final close sent by `finish`, plus any data
            // enqueued before it), honoring each message's deadline, before ending.
            while let Some(command) = queue.next().await {
                interceptor.send(command).await;
            }
        });

        Self {
            tx,
            handle: AbortOnDropHandle::new(handle),
        }
    }

    /// Helper to enqueue `command` with a release `delay` (counted from now) on [`Self::tx`].
    async fn send(&self, command: InterceptorCommand, delay: Duration) {
        let _ = self
            .tx
            .send(QueuedInterceptorMessage {
                command,
                deadline: Instant::now() + delay,
            })
            .await;
    }

    /// Sends the last `command` through [`Self::tx`] to the background task.
    ///
    /// We [`AbortOnDropHandle::detach`] here to give the task enough time to process this message,
    /// afterwards it'll try to read another message, see that the channel is closed (we drop `tx`
    /// here) and this will end the task.
    async fn finish(self, command: InterceptorCommand, delay: Duration) {
        let Self { tx, handle } = self;

        let _ = tx
            .send(QueuedInterceptorMessage {
                command,
                deadline: Instant::now() + delay,
            })
            .await;
        drop(handle.detach());
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
            queue.send(InterceptorCommand::Data(bytes), delay).await;
            true
        } else {
            false
        }
    }

    /// Sends a command to an [`Interceptor`] through the delayed per-interceptor queue.
    pub(crate) async fn queue_interceptor_command(
        &mut self,
        id: InterceptorId,
        command: InterceptorCommand,
        delay: Duration,
    ) -> bool {
        if let Some(queue) = self.interceptor_read_queues.get(&id) {
            queue.send(command, delay).await;
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
        command: InterceptorCommand,
        delay: Duration,
    ) {
        if let Some(queue) = self.interceptor_read_queues.remove(&id) {
            queue.finish(command, delay).await;
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
