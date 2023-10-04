use std::collections::VecDeque;

use thiserror::Error;

use crate::protocol::MessageId;

#[derive(Error, Debug)]
#[error("request queue is empty")]
pub struct RequestQueueEmpty;

/// A request fifo used to match agent responses with layer requests.
///
/// The layer annotates each message with unique [`MessageId`],
/// which allows it to easliy match requests with responses from the internal proxy.
/// However, the agent does not use any mechanism like this.
/// Instead, its components (e.g. file manager) handle requests sequentially.
#[derive(Debug, Default)]
pub struct RequestQueue {
    inner: VecDeque<MessageId>,
}

impl RequestQueue {
    /// Save request for later.
    pub fn insert(&mut self, id: MessageId) {
        self.inner.push_back(id);
    }

    /// Retrieve and remove oldest request.
    pub fn get(&mut self) -> Result<MessageId, RequestQueueEmpty> {
        self.inner.pop_front().ok_or(RequestQueueEmpty)
    }
}
