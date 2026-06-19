use std::{ops::Not, time::Duration};

use bytes::Bytes;
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};

use super::Interceptor;
use crate::{
    background_tasks::TaskSender,
    proxies::outgoing::{InterceptorId, OutgoingProxy},
};

struct QueuedInterceptorMessage {
    bytes: Bytes,
    delay: Duration,
}

pub(in crate::proxies::outgoing) struct InterceptorReadQueue {
    tx: mpsc::Sender<QueuedInterceptorMessage>,
    handle: JoinHandle<()>,
}

impl InterceptorReadQueue {
    pub(in crate::proxies::outgoing) fn new(
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

        Self { tx, handle }
    }

    pub(in crate::proxies::outgoing) async fn send(&self, bytes: Bytes, delay: Duration) {
        let _ = self
            .tx
            .send(QueuedInterceptorMessage { bytes, delay })
            .await;
    }

    pub(in crate::proxies::outgoing) fn abort(self) {
        self.handle.abort();
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
            queue.send(bytes, delay).await;
        }
    }

    pub(crate) fn abort_interceptor_read_queue(&mut self, id: &InterceptorId) -> bool {
        if let Some(queue) = self.interceptor_read_queues.remove(id) {
            queue.abort();
            true
        } else {
            false
        }
    }

    pub(crate) fn abort_all_interceptor_read_queues(&mut self) {
        for queue in std::mem::take(&mut self.interceptor_read_queues).into_values() {
            queue.abort();
        }
    }
}
