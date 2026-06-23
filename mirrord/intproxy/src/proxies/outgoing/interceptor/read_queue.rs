use std::{ops::Not, time::Duration};

use bytes::Bytes;
use tokio::{sync::mpsc, time::sleep};
use tokio_util::task::AbortOnDropHandle;

use super::Interceptor;
use crate::{
    background_tasks::TaskSender,
    proxies::outgoing::{InterceptorId, OutgoingProxy},
};

struct QueuedInterceptorMessage {
    bytes: Bytes,
    delay: Duration,
}

pub(crate) struct InterceptorReadQueue {
    tx: mpsc::Sender<QueuedInterceptorMessage>,
    handle: AbortOnDropHandle<()>,
}

impl InterceptorReadQueue {
    pub(crate) fn new(
        id: InterceptorId,
        interceptor: TaskSender<Interceptor>,
        channel_size: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedInterceptorMessage>(channel_size);

        let handle = tokio::spawn(async move {
            while let Some(QueuedInterceptorMessage { bytes, delay }) = rx.recv().await {
                if delay.is_zero().not() {
                    tracing::trace!(%id, ?delay, "Delaying outgoing message to layer");
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

    pub(crate) async fn send(&self, bytes: Bytes, delay: Duration) {
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
    pub(crate) async fn finish(self, bytes: Bytes, delay: Duration) {
        let Self { tx, handle } = self;

        let _ = tx.send(QueuedInterceptorMessage { bytes, delay }).await;
        let _ = handle.detach();
    }
}

impl OutgoingProxy {
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

    pub(crate) fn abort_interceptor_read_queue(&mut self, id: &InterceptorId) -> bool {
        self.interceptor_read_queues.remove(id).is_some()
    }

    pub(crate) fn abort_all_interceptor_read_queues(&mut self) {
        self.interceptor_read_queues.clear();
    }
}
