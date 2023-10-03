use std::collections::VecDeque;

use crate::error::{IntProxyError, Result};

/// A request fifo used to match agent responses with layer requests.
///
/// The layer annotates each message with unique [`MessageId`],
/// which allows it to easliy match requests with responses from the internal proxy.
/// However, the agent does not use any mechanism like this.
/// Instead, its components (e.g. file manager) handle requests sequentially.
#[derive(Debug)]
pub struct RequestQueue<T> {
    inner: VecDeque<T>,
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
    pub fn insert(&mut self, request: T) {
        self.inner.push_back(request);
    }

    /// Retrieve and remove oldest request.
    pub fn get(&mut self) -> Result<T> {
        self.inner
            .pop_front()
            .ok_or(IntProxyError::RequestQueueEmpty)
    }
}
