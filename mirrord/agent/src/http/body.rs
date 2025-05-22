use std::{
    pin::Pin,
    task::{Context, Poll},
    vec,
};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt};
use hyper::body::{Body, Frame, Incoming};

/// [`Body`] that consist of some first [`Frame`]s that were already received,
/// and the optional rest of the body.
pub struct RolledBackBody {
    pub head: vec::IntoIter<Frame<Bytes>>,
    pub tail: Option<Incoming>,
}

impl Body for RolledBackBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();

        if let Some(frame) = this.head.next() {
            return Poll::Ready(Some(Ok(frame)));
        }

        let Some(tail) = this.tail.as_mut() else {
            return Poll::Ready(None);
        };

        Pin::new(tail).poll_frame(cx)
    }
}

impl From<RolledBackBody> for BoxBody<Bytes, hyper::Error> {
    fn from(value: RolledBackBody) -> Self {
        value.map_err(|_| unreachable!()).boxed()
    }
}
