use std::collections::VecDeque;

use crate::protocol::MessageId;

#[derive(Default, Debug)]
pub struct RequestQueue(VecDeque<MessageId>);

impl RequestQueue {
    pub fn save_request_id(&mut self, id: MessageId) {
        self.0.push_back(id);
    }

    pub fn get_request_id(&mut self) -> Option<MessageId> {
        self.0.pop_front()
    }
}
