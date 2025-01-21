use std::{
    convert::Infallible,
    fmt,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use hyper::body::{Body, Frame};
use mirrord_protocol::tcp::{InternalHttpBody, InternalHttpBodyFrame};
use tokio::sync::mpsc::{self, Receiver};

/// Cheaply cloneable [`Body`] implementation that reads [`Frame`]s from an [`mpsc::channel`].
///
/// # Clone behavior
///
/// All instances acquired via [`Clone`] share the [`mpsc::Receiver`] and a vector of previously
/// read frames. Each instance maintains its own position in the shared vector, and a new clone
/// starts at 0.
///
/// When polled with [`Body::poll_frame`], an instance tries to return a cached frame.
///
/// Thanks to this, each clone returns all frames from the start when polled with
/// [`Body::poll_frame`]. As you'd expect from a cloneable [`Body`] implementation.
pub struct StreamingBody {
    /// Shared with instances acquired via [`Clone`].
    ///
    /// Allows the clones to access previously fetched [`Frame`]s.
    shared_state: Arc<Mutex<(Receiver<InternalHttpBodyFrame>, Vec<InternalHttpBodyFrame>)>>,
    /// Index of the next frame to return from the buffer, not shared with other instances acquired
    /// via [`Clone`].
    ///
    /// If outside of the buffer, we need to poll the stream to get the next frame.
    idx: usize,
}

impl StreamingBody {
    /// Creates a new instance of this [`Body`].
    ///
    /// It will first read all frames from the vector given as `first_frames`.
    /// Following frames will be fetched from the given `rx`.
    pub fn new(
        rx: Receiver<InternalHttpBodyFrame>,
        first_frames: Vec<InternalHttpBodyFrame>,
    ) -> Self {
        Self {
            shared_state: Arc::new(Mutex::new((rx, first_frames))),
            idx: 0,
        }
    }
}

impl Clone for StreamingBody {
    fn clone(&self) -> Self {
        Self {
            shared_state: self.shared_state.clone(),
            // Setting idx to 0 in order to replay the previous frames.
            idx: 0,
        }
    }
}

impl Body for StreamingBody {
    type Data = Bytes;

    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let mut guard = this.shared_state.lock().unwrap();

        if let Some(frame) = guard.1.get(this.idx) {
            this.idx += 1;
            return Poll::Ready(Some(Ok(frame.clone().into())));
        }

        match std::task::ready!(guard.0.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(frame) => {
                guard.1.push(frame.clone());
                this.idx += 1;
                Poll::Ready(Some(Ok(frame.into())))
            }
        }
    }
}

impl Default for StreamingBody {
    fn default() -> Self {
        let (_, dummy_rx) = mpsc::channel(1); // `mpsc::channel` panics on capacity 0
        Self {
            shared_state: Arc::new(Mutex::new((dummy_rx, Default::default()))),
            idx: 0,
        }
    }
}

impl From<Vec<u8>> for StreamingBody {
    fn from(value: Vec<u8>) -> Self {
        let (_, dummy_rx) = mpsc::channel(1); // `mpsc::channel` panics on capacity 0
        let frames = vec![InternalHttpBodyFrame::Data(value)];
        Self::new(dummy_rx, frames)
    }
}

impl From<InternalHttpBody> for StreamingBody {
    fn from(value: InternalHttpBody) -> Self {
        let (_, dummy_rx) = mpsc::channel(1); // `mpsc::channel` panics on capacity 0
        Self::new(dummy_rx, value.0.into_iter().collect())
    }
}

impl From<Receiver<InternalHttpBodyFrame>> for StreamingBody {
    fn from(value: Receiver<InternalHttpBodyFrame>) -> Self {
        Self::new(value, Default::default())
    }
}

impl fmt::Debug for StreamingBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("StreamingBody");
        s.field("idx", &self.idx);

        match self.shared_state.try_lock() {
            Ok(guard) => {
                s.field("frame_rx_closed", &guard.0.is_closed());
                s.field("cached_frames", &guard.1);
            }
            Err(error) => {
                s.field("lock_error", &error);
            }
        }

        s.finish()
    }
}
