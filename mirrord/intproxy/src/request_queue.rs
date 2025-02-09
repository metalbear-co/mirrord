//! A request fifo used by the internal proxy to match agent responses with layer requests.
//!
//! The layer annotates each message with unique [`MessageId`],
//! which allows it to match responses from the internal proxy with awaiting requests.
//! However, the agent does not use any mechanism like this.
//! Instead, its components (e.g. file manager) handle requests of the same kind (e.g.
//! [`FileRequest`](mirrord_protocol::codec::FileRequest)s) sequentially. The internal proxy relies
//! on this behavior and stores [`MessageId`]s of layer's requests in multiple queues. Upon
//! receiving a response from the agent, correct [`MessageId`] is taken from the right queue.
//!
//! Additionaly, single internal proxy handles multiple layer
//! instances (coming from forks). This fifo stores their [`LayerId`]s as well.

use std::{collections::VecDeque, fmt};

use mirrord_intproxy_protocol::{LayerId, MessageId};
use tracing::Level;

/// A queue used to match agent responses with layer requests.
/// A single queue can be used for multiple types of requests only if the agent preserves order
/// between them.
pub struct RequestQueue<T = ()> {
    inner: VecDeque<(MessageId, LayerId, T)>,
}

impl<T> Default for RequestQueue<T> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for RequestQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestQueue")
            .field("queue_len", &self.inner.len())
            .field("front", &self.inner.front())
            .field("back", &self.inner.back())
            .finish()
    }
}

impl<T: Default + fmt::Debug> RequestQueue<T> {
    /// Save the request at the end of this queue.
    #[tracing::instrument(level = Level::TRACE)]
    pub fn push_back(&mut self, message_id: MessageId, layer_id: LayerId) {
        self.inner
            .push_back((message_id, layer_id, Default::default()));
    }
}

impl<T: fmt::Debug> RequestQueue<T> {
    /// Retrieve and remove a request from the front of this queue.
    #[tracing::instrument(level = Level::TRACE)]
    pub fn pop_front(&mut self) -> Option<(MessageId, LayerId)> {
        let (message_id, layer_id, _) = self.inner.pop_front()?;
        Some((message_id, layer_id))
    }

    /// Save the request at the end of this queue.
    #[tracing::instrument(level = Level::TRACE)]
    pub fn push_back_with_data(&mut self, message_id: MessageId, layer_id: LayerId, data: T) {
        self.inner.push_back((message_id, layer_id, data));
    }

    /// Retrieve and remove a request from the front of this queue.
    #[tracing::instrument(level = Level::TRACE)]
    pub fn pop_front_with_data(&mut self) -> Option<(MessageId, LayerId, T)> {
        self.inner.pop_front()
    }

    /// Clear request queue.
    #[tracing::instrument(level = Level::TRACE)]
    pub fn clear(&mut self) {
        self.inner.clear()
    }
}
