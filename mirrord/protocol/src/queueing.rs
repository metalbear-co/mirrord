use std::{
    fmt,
    hash::Hash,
    mem::{Discriminant, discriminant},
};

use crate::ConnectionId;

/// A trait implemented on message types that dictates how queueing
/// should work for different message types.
pub trait Queueable: Sized {
    /// Returns the queue id that should be used for this message.
    /// Messages with the same queue id will end up in the same queue
    /// and thus be processed sequentially. Messages with different
    /// queue ids will end up in different queues and may be processed
    /// out of order. This is used for fair scheduling between
    /// different logical data streams, e.g. tcp data packets will use
    /// their connection id as the queueid. Most other message types
    /// should use the enum discriminant as the queue id.
    fn queue_id(&self) -> QueueId<Self>;
}

pub enum QueueId<T> {
    /// For messages which have all their instances in a single queue
    Normal(Discriminant<T>),

    /// For tcp data messages (we want to split bandwidth equally between tcp connections)
    Tcp(ConnectionId),
}

// Need these because derives add unnecessary bounds on T
impl<T> Clone for QueueId<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Normal(arg0) => Self::Normal(arg0.clone()),
            Self::Tcp(arg0) => Self::Tcp(arg0.clone()),
        }
    }
}
impl<T> Hash for QueueId<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        discriminant(self).hash(state);
        match self {
            QueueId::Normal(discriminant) => discriminant.hash(state),
            QueueId::Tcp(id) => id.hash(state),
        }
    }
}

impl<T> PartialEq for QueueId<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Normal(l0), Self::Normal(r0)) => l0 == r0,
            (Self::Tcp(l0), Self::Tcp(r0)) => l0 == r0,
            _ => false,
        }
    }
}

impl<T> fmt::Debug for QueueId<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normal(arg0) => f.debug_tuple("Normal").field(arg0).finish(),
            Self::Tcp(arg0) => f.debug_tuple("Tcp").field(arg0).finish(),
        }
    }
}

impl<T> Eq for QueueId<T> {}
