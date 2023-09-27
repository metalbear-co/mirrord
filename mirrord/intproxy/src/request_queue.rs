use std::collections::VecDeque;

use crate::protocol::MessageId;

#[derive(Debug, Default)]
pub struct RequestQueue {
    inner: VecDeque<MessageId>,
}

impl RequestQueue {
    pub fn save_request_id(&mut self, id: MessageId) {
        self.inner.push_back(id);
    }

    pub fn get_request_id(&mut self) -> Option<MessageId> {
        self.inner.pop_front()
    }
}
