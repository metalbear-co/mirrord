use std::collections::VecDeque;

use crate::{
    error::{IntProxyError, Result},
    protocol::MessageId,
};

#[derive(Debug)]
pub struct RequestWithId<T> {
    pub id: MessageId,
    pub request: T,
}

/// A request fifo used to match agent responses with layer requests.
///
/// The layer annotates each message with unique [`MessageId`],
/// which allows it to easliy match requests with responses from the internal proxy.
/// However, the agent does not use any mechanism like this.
/// Instead, its components (e.g. file manager) handle requests sequentially.
#[derive(Debug)]
pub struct RequestQueue<T> {
    inner: VecDeque<RequestWithId<T>>,
}

impl<T> Default for RequestQueue<T> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl<T> RequestQueue<T> {
    /// Save request for later.
    pub fn insert(&mut self, request: T, id: MessageId) {
        self.inner.push_back(RequestWithId { id, request });
    }

    /// Retrieve and remove oldest request.
    pub fn get(&mut self) -> Result<RequestWithId<T>> {
        self.inner
            .pop_front()
            .ok_or(IntProxyError::RequestQueueEmpty)
    }
}
