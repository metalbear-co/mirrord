use std::collections::VecDeque;

use crate::protocol::MessageId;

/// A [`MessageId`] fifo used to match agent responses with layer requests.
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
    /// Save [`MessageId`] for later.
    pub fn save_request_id(&mut self, id: MessageId) {
        self.inner.push_back(id);
    }

    /// Retrieve and remove oldest [`MessageId`].
    pub fn get_request_id(&mut self) -> Option<MessageId> {
        self.inner.pop_front()
    }
}
