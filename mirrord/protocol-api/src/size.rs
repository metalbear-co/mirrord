use std::collections::VecDeque;

use bytes::Bytes;
use hyper::HeaderMap;
use mirrord_protocol::{
    LogMessage,
    tcp::{IncomingTrafficTransportType, InternalHttpBodyFrame, InternalHttpBodyNew},
};

use crate::traffic::{
    BodyError, ReceivedBody, TunneledIncoming, TunneledIncomingInner, TunneledOutgoing,
};

/// Trait for types that may have a heap-allocated size.
///
/// Used with [`crate::fifo::Fifo`] to track memory usage and apply backpressure to
/// [`mirrord_protocol`] connections.
pub trait HeapSize {
    /// Returns an estimate of heap-allocated size of self.
    fn heap_size(&self) -> usize;
}

impl HeapSize for Bytes {
    fn heap_size(&self) -> usize {
        self.len()
    }
}

impl HeapSize for String {
    fn heap_size(&self) -> usize {
        self.capacity()
    }
}

impl HeapSize for HeaderMap {
    fn heap_size(&self) -> usize {
        self.iter()
            .map(|(k, v)| k.as_str().len() + v.as_bytes().len())
            .sum()
    }
}

impl HeapSize for InternalHttpBodyFrame {
    fn heap_size(&self) -> usize {
        match self {
            Self::Data(data) => data.0.len(),
            Self::Trailers(trailers) => trailers.heap_size(),
        }
    }
}

impl HeapSize for InternalHttpBodyNew {
    fn heap_size(&self) -> usize {
        self.frames.heap_size()
    }
}

impl HeapSize for BodyError {
    fn heap_size(&self) -> usize {
        self.0.as_ref().map(HeapSize::heap_size).unwrap_or_default()
    }
}

impl HeapSize for IncomingTrafficTransportType {
    fn heap_size(&self) -> usize {
        match self {
            Self::Tcp => 0,
            Self::Tls {
                alpn_protocol,
                server_name,
            } => alpn_protocol.heap_size() + server_name.heap_size(),
        }
    }
}

impl HeapSize for TunneledIncoming {
    fn heap_size(&self) -> usize {
        self.transport.heap_size() + self.inner.heap_size()
    }
}

impl HeapSize for TunneledIncomingInner {
    fn heap_size(&self) -> usize {
        match self {
            Self::Raw(..) => 0,
            Self::Http(request) => request.request.heap_size(),
        }
    }
}

impl HeapSize for ReceivedBody {
    fn heap_size(&self) -> usize {
        self.head.heap_size()
    }
}

impl<B: HeapSize> HeapSize for hyper::Request<B> {
    fn heap_size(&self) -> usize {
        self.headers().heap_size() + self.body().heap_size()
    }
}

impl<A> HeapSize for TunneledOutgoing<A> {
    fn heap_size(&self) -> usize {
        0
    }
}

impl HeapSize for LogMessage {
    fn heap_size(&self) -> usize {
        self.message.heap_size()
    }
}

impl HeapSize for u8 {
    fn heap_size(&self) -> usize {
        0
    }
}

impl<T: HeapSize> HeapSize for Vec<T> {
    fn heap_size(&self) -> usize {
        self.iter().map(HeapSize::heap_size).sum::<usize>() + self.capacity() * size_of::<T>()
    }
}

impl<T: HeapSize> HeapSize for VecDeque<T> {
    fn heap_size(&self) -> usize {
        self.iter().map(HeapSize::heap_size).sum::<usize>() + self.capacity() * size_of::<T>()
    }
}

impl<T: HeapSize, E: HeapSize> HeapSize for Result<T, E> {
    fn heap_size(&self) -> usize {
        self.as_ref()
            .map_or_else(HeapSize::heap_size, HeapSize::heap_size)
    }
}

impl<T: HeapSize> HeapSize for Option<T> {
    fn heap_size(&self) -> usize {
        self.as_ref().map(HeapSize::heap_size).unwrap_or_default()
    }
}
