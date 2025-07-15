use bytes::Bytes;
use hyper::body::Frame;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::sync::broadcast;

use crate::incoming::{connection::copy_bidirectional::CowBytes, IncomingStreamItem};

/// Utility wrapper over an optiona [`broadcast::Sender`].
///
/// 1. The sender is dropped as soon as we detect that there are no receivers left. This allows for
///    dropping the whole channel as soon as its no longer needed.
/// 2. Exposes methods that make it easier to do expensive cloning only when necessary
///    ([`Self::send_data`], [`Self::send_frame`]).
#[derive(Clone)]
pub struct OptionalBroadcast(Option<broadcast::Sender<IncomingStreamItem>>);

impl OptionalBroadcast {
    pub fn send_item(&mut self, item: IncomingStreamItem) {
        let Some(tx) = &self.0 else {
            return;
        };

        if tx.send(item).is_err() {
            self.0 = None;
        }
    }

    pub fn send_data(&mut self, data: CowBytes<'_>) {
        let Some(tx) = &self.0 else {
            return;
        };

        let item = match data {
            CowBytes::Owned(bytes) => IncomingStreamItem::Data(bytes),
            CowBytes::Borrowed(slice) => IncomingStreamItem::Data(slice.to_vec().into()),
        };
        if tx.send(item).is_err() {
            self.0 = None;
        }
    }

    pub fn send_frame(&mut self, frame: &Frame<Bytes>) {
        let Some(tx) = &self.0 else {
            return;
        };

        let frame = if let Some(bytes) = frame.data_ref() {
            InternalHttpBodyFrame::Data(bytes.clone().into())
        } else if let Some(trailers) = frame.trailers_ref() {
            InternalHttpBodyFrame::Trailers(trailers.clone())
        } else {
            panic!("malformed frame")
        };
        if tx.send(IncomingStreamItem::Frame(frame)).is_err() {
            self.0 = None;
        }
    }
}

impl From<Option<broadcast::Sender<IncomingStreamItem>>> for OptionalBroadcast {
    fn from(value: Option<broadcast::Sender<IncomingStreamItem>>) -> Self {
        Self(value)
    }
}
