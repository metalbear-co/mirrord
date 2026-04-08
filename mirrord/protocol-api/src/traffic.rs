//! Various types of traffic that can be tunneled through [`mirrord_protocol`].

use std::{
    collections::VecDeque,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::StreamExt;
use http_body::{Body, Frame};
use http_body_util::combinators::BoxBody;
use mirrord_protocol::tcp::{
    IncomingTrafficTransportType, InternalHttpBodyFrame, InternalHttpBodyNew,
};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::fifo::{FifoClosed, FifoSink, FifoStream};

/// Bidirectional data tunnel established through [`mirrord_protocol`].
#[derive(Debug)]
pub struct TunneledData {
    /// Can be used to write data.
    ///
    /// Should be dropped when you stop writing, unless this is a mirrored connection.
    /// In that case, it's already closed.
    pub sink: FifoSink<Bytes>,
    /// [`Future`] that resolves when [`Self::sink`] is closed.
    pub sink_closed: FifoClosed<Bytes>,
    /// Can be used to read data.
    ///
    /// Should be dropped when you stop reading.
    pub stream: FifoStream<Bytes>,
}

/// Outgoing connection tunneled through [`mirrord_protocol`].
#[derive(Debug)]
pub struct TunneledOutgoing<A> {
    /// Address of the original server socket.
    pub remote_local_addr: A,
    /// Address of the original peer socket.
    pub remote_peer_addr: A,
    /// Can be used to send and receive data.
    pub data: TunneledData,
}

#[derive(Debug)]
pub struct TunneledRequest {
    /// Incoming HTTP request.
    ///
    /// Boxed due to size.
    pub request: Box<hyper::Request<ReceivedBody>>,
    /// Can be used to respond to the request.
    ///
    /// Always closed for mirrored requests.
    pub response_tx: oneshot::Sender<hyper::Response<BoxBody<Bytes, ()>>>,
    /// Can be used to receive the HTTP upgrade.
    ///
    /// The upgrade will never be sent before all response frames are processed.
    pub upgrade_rx: oneshot::Receiver<TunneledData>,
}

/// Incoming traffic (a connection or an HTTP request) tunneled through [`mirrord_protocol`].
#[derive(Debug)]
pub struct TunneledIncoming {
    pub inner: TunneledIncomingInner,
    pub transport: IncomingTrafficTransportType,
    /// Address of the original server socket.
    pub remote_local_addr: SocketAddr,
    /// Address of the original peer socket.
    pub remote_peer_addr: SocketAddr,
}

#[derive(Debug)]
pub enum TunneledIncomingInner {
    Raw(TunneledData),
    Http(TunneledRequest),
}

/// Body of an HTTP request tunneled through a [`mirrord_protocol`] connection.
///
/// Can be used as [`Body`] if you need a request to be sent with [`hyper::client`],
/// or as [`Stream`](futures::Stream) if you need a transparent [`mirrord_protocol`] proxy.
#[derive(Debug)]
pub struct ReceivedBody {
    pub head: VecDeque<InternalHttpBodyFrame>,
    pub tail: Option<FifoStream<Result<InternalHttpBodyNew, BodyError>>>,
}

impl Body for ReceivedBody {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();

        loop {
            if let Some(frame) = this.head.pop_front() {
                if this.head.is_empty() {
                    this.head = Default::default();
                }
                break Poll::Ready(Some(Ok(frame.into())));
            }

            let Some(tail) = this.tail.as_mut() else {
                break Poll::Ready(None);
            };
            match std::task::ready!(tail.poll_next_unpin(cx)) {
                Some(Ok(body)) => {
                    this.head = body.frames.into();
                    if body.is_last {
                        this.tail = None;
                    }
                }
                Some(Err(error)) => {
                    this.tail = None;
                    break Poll::Ready(Some(Err(error)));
                }
                None => {
                    this.tail = None;
                    break Poll::Ready(None);
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.head.is_empty() && self.tail.is_none()
    }
}

/// Error that occurs when the [`mirrord_protocol`] server fails to read body of a tunneled HTTP
/// request.
#[derive(Error, Debug, Clone)]
#[error("mirrord-protocol server failed to read request body: {}", .0.as_deref().unwrap_or("<no-message>"))]
pub struct BodyError(pub Option<String>);
