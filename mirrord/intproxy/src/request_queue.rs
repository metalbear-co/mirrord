use std::collections::VecDeque;

use thiserror::Error;

#[derive(Error, Debug)]
#[error("request queue {0} is empty")]
pub struct RequestQueueEmpty(&'static str);

/// A request fifo used to match agent responses with layer requests.
///
/// The layer annotates each message with unique [`MessageId`],
/// which allows it to easliy match requests with responses from the internal proxy.
/// However, the agent does not use any mechanism like this.
/// Instead, its components (e.g. file manager) handle requests sequentially.
#[derive(Debug)]
pub struct RequestQueue<T> {
    inner: VecDeque<T>,
    name: &'static str,
}

impl<T> RequestQueue<T> {
    pub fn new(name: &'static str) -> Self {
        Self {
            inner: Default::default(),
            name,
        }
    }

    /// Save request for later.
    pub fn insert(&mut self, request: T) {
        self.inner.push_back(request);
    }

    /// Retrieve and remove oldest request.
    pub fn get(&mut self) -> Result<T, RequestQueueEmpty> {
        self.inner.pop_front().ok_or(RequestQueueEmpty(self.name))
    }
}
