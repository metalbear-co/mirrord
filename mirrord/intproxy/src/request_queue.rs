//! A request fifo used by the internal proxy to match agent responses with layer requests.
//!
//! The layer annotates each message with unique [`MessageId`],
//! which allows it to match responses from the internal proxy with awaiting requests.
//! However, the agent does not use any mechanism like this.
//! Instead, its components (e.g. file manager) handle requests of the same kind (e.g.
//! [`FileRequest`](mirrord_protocol::codec::FileRequest)s) sequentially. The internal proxy relies
//! on this behavior and stores [`MessageId`]s of layer's requests in multiple queues. Upon
//! receiving a response from the agent, correct [`MessageId`] is taken from the right queue.

use std::collections::VecDeque;

use thiserror::Error;

use crate::protocol::MessageId;

/// Erorr returned when the proxy attempts to retrieve [`MessageId`] of a request corresponding to a
/// response received from the agent, but the [`RequestQueue`] is empty. This error should never
/// happen.
#[derive(Error, Debug)]
#[error("request queue is empty")]
pub struct RequestQueueEmpty;

/// A queue used to match agent responses with layer requests.
/// A single queue can be used for multiple types of requests only if the agent preserves order
/// between them.
#[derive(Debug, Default)]
pub struct RequestQueue {
    inner: VecDeque<MessageId>,
}

impl RequestQueue {
    /// Save the request at the end of this queue.
    pub fn insert(&mut self, id: MessageId) {
        self.inner.push_back(id);
    }

    /// Retrieve and remove a requests from the front of this queue.
    pub fn get(&mut self) -> Result<MessageId, RequestQueueEmpty> {
        self.inner.pop_front().ok_or(RequestQueueEmpty)
    }
}
