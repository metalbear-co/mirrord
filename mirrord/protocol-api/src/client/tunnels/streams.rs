use std::{
    collections::VecDeque,
    fmt,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
use futures::{
    FutureExt, Stream, StreamExt,
    stream::{self, AbortHandle, Abortable, Once, SelectAll},
};
use http_body::Body;
use http_body_util::combinators::BoxBody;
use hyper::{StatusCode, http::response::Parts};
use mirrord_protocol::{
    Payload,
    tcp::{
        HTTP_CHUNKED_RESPONSE_VERSION, HTTP_FRAMED_VERSION, InternalHttpBody,
        InternalHttpBodyFrame, InternalHttpBodyNew, InternalHttpResponse,
    },
};
use strum_macros::EnumDiscriminants;
use tokio::sync::oneshot;

use crate::{
    client::tunnels::TunnelId,
    fifo::{FifoClosed, FifoStream},
    tagged::Tagged,
    traffic::BodyError,
};

/// A traffic stream event yielded by [`InternalStreams`].
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum StreamEvent {
    /// Received or produced HTTP response.
    ///
    /// A default [`StatusCode::BAD_GATEWAY`] response is produced if the stream is unable to
    /// receive the response from the client.
    Response(InternalHttpResponse<InternalResponseBody>),
    /// Received HTTP response body chunk.
    ResponseBodyChunk(InternalHttpBodyNew),
    /// Failure when receiving the next HTTP response body chunk.
    ResponseBodyError,
    /// Closed inbound data [`Fifo`](crate::fifo::Fifo).
    InboundDataClosed,
    /// New data received from an outbound [`Fifo`](crate::fifo::Fifo).
    OutboundData(Bytes),
    /// Closed outbound data [`Fifo`](crate::fifo::Fifo).
    OutboundDataClosed,
    /// Closed request body [`Fifo`](crate::fifo::Fifo).
    InboundBodyClosed,
}

/// Set of traffic streams, internal to [`super::TrafficTunnels`].
///
/// Allows for polling multiple event sources at the same time.
///
/// The streams make progress only when this set is polled with [`Stream::poll_next`].
///
/// # Stream abort
///
/// Pushing a new stream into this set returns an [`AbortOnDrop`] handle, that can be prematurely
/// abort the stream. An aborted stream immediately stops yiedling [`StreamEvent`]s.
#[derive(Default)]
pub struct InternalStreams(SelectAll<Tagged<Abortable<InternalStream>, TunnelId>>);

impl InternalStreams {
    /// Pushes a new HTTP response stream into this set.
    ///
    /// # Events
    ///
    /// The stream will yield different [`StreamEvent`]s, depending on the provided
    /// [`ResponseMode`].
    ///
    /// ## [`ResponseMode::Legacy`]
    ///
    /// The stream will yield exactly one event - [`StreamEvent::Response`] with
    /// [`InternalResponseBody::Legacy`].
    ///
    /// ## [`ResponseMode::Framed`]
    ///
    /// The stream will yield exactly one event - [`StreamEvent::Response`] with
    /// [`InternalResponseBody::Framed`].
    ///
    /// ## [`ResponseMode::Chunked`]
    ///
    /// The stream will first yield [`StreamEvent::Response`] with
    /// [`InternalResponseBody::Chunked`]. Then, it will yield an unspecified number of
    /// [`StreamEvent::ResponseBodyChunk`]. Then, it will yield *at most* one
    /// [`StreamEvent::ResponseBodyError`].
    ///
    /// The last produced [`StreamEvent`] will always be either:
    /// 1. [`StreamEvent::Response`] with [`InternalResponseBody::Chunked`] and
    ///    [`InternalHttpBodyNew::is_last`] set; OR
    /// 2. [`StreamEvent::ResponseBodyChunk`] with [`InternalHttpBodyNew::is_last`] set; OR
    /// 3. [`StreamEvent::ResponseBodyError`]
    ///
    /// # Fallback response
    ///
    /// If the stream fails to receive the response, it will yield a single
    /// [`StreamEvent::Response`] with correct body variant and [`StatusCode::BAD_GATEWAY`] status.
    ///
    /// # Params
    ///
    /// * `tag` - attached to each item produced by the new stream
    /// * `rx` - polled for [`hyper::Response`] from the client
    /// * `response_mode` - used to pick the correct body variant
    /// * `http_version` - used when producing a fallback response
    pub fn push_response(
        &mut self,
        tag: TunnelId,
        rx: oneshot::Receiver<hyper::Response<BoxBody<Bytes, ()>>>,
        response_mode: ResponseMode,
        http_version: hyper::Version,
    ) -> AbortOnDrop {
        let stream = match response_mode {
            ResponseMode::Legacy => {
                InternalStream::ResponseFull(stream::once(FullResponseFut::AwaitHead {
                    rx,
                    http_version,
                    legacy: true,
                }))
            }
            ResponseMode::Framed => {
                InternalStream::ResponseFull(stream::once(FullResponseFut::AwaitHead {
                    rx,
                    http_version,
                    legacy: false,
                }))
            }
            ResponseMode::Chunked => {
                InternalStream::ResponseChunked(ChunkedResponseStream::AwaitHead {
                    rx,
                    http_version,
                })
            }
        };
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.0.push(Tagged {
            inner: Abortable::new(stream, abort_registration),
            tag,
        });
        AbortOnDrop(abort_handle)
    }

    /// Pushes a new inbound data sink watcher stream into this set.
    ///
    /// The stream will wait for the [`FifoClosed`] to complete, and then yield exactly one event -
    /// [`StreamEvent::InboundDataClosed`].
    pub fn push_inbound_data_closed(
        &mut self,
        tag: TunnelId,
        closed: FifoClosed<Bytes>,
    ) -> AbortOnDrop {
        let stream = InternalStream::InboundDataClosed(stream::once(InboundDataClosedFut(closed)));
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.0.push(Tagged {
            inner: Abortable::new(stream, abort_registration),
            tag,
        });
        AbortOnDrop(abort_handle)
    }

    /// Pushes a new inbound body sink watcher stream into this set.
    ///
    /// The stream will wait for the [`FifoClosed`] to complete, and then yield exactly one event -
    /// [`StreamEvent::InboundBodyClosed`].
    pub fn push_inbound_body_closed(
        &mut self,
        tag: TunnelId,
        closed: FifoClosed<Result<InternalHttpBodyNew, BodyError>>,
    ) -> AbortOnDrop {
        let stream = InternalStream::InboundBodyClosed(stream::once(InboundBodyClosedFut(closed)));
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.0.push(Tagged {
            inner: Abortable::new(stream, abort_registration),
            tag,
        });
        AbortOnDrop(abort_handle)
    }

    /// Pushes a new outbound data stream into this set.
    ///
    /// The stream will first yield an unspecified number of [`StreamEvent::OutboundData`], and none
    /// of the data will be empty. Then, it will yield a single
    /// [`StreamEvent::OutboundDataClosed`].
    pub fn push_outbound_data(&mut self, tag: TunnelId, data: FifoStream<Bytes>) -> AbortOnDrop {
        let stream = InternalStream::OutboundData(OutboundDataStream(Some(data)));
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.0.push(Tagged {
            inner: Abortable::new(stream, abort_registration),
            tag,
        });
        AbortOnDrop(abort_handle)
    }
}

impl Stream for InternalStreams {
    type Item = (TunnelId, StreamEvent);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().0.poll_next_unpin(cx)
    }
}

impl fmt::Debug for InternalStreams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut l = f.debug_list();
        for stream in self.0.iter() {
            l.entry(&(stream.tag, &stream.inner));
        }
        l.finish()
    }
}

/// Guard for a stream pushed to [`InternalStreams`].
///
/// Aborts the stream when dropped. An aborted stream immediately stops yiedling [`StreamEvent`]s.
#[derive(Debug)]
pub struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug)]
enum InternalStream {
    ResponseFull(Once<FullResponseFut>),
    ResponseChunked(ChunkedResponseStream),
    InboundDataClosed(Once<InboundDataClosedFut>),
    OutboundData(OutboundDataStream),
    InboundBodyClosed(Once<InboundBodyClosedFut>),
}

impl Stream for InternalStream {
    type Item = StreamEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this {
            Self::ResponseFull(stream) => stream.poll_next_unpin(cx),
            Self::ResponseChunked(stream) => stream.poll_next_unpin(cx),
            Self::InboundDataClosed(stream) => stream.poll_next_unpin(cx),
            Self::OutboundData(stream) => stream.poll_next_unpin(cx),
            Self::InboundBodyClosed(stream) => stream.poll_next_unpin(cx),
        }
    }
}

/// [`Future`] that receives a [`hyper::Response`].
///
/// Collects the full body into [`InternalResponseBody::Legacy`] or
/// [`InternalResponseBody::Framed`].
enum FullResponseFut {
    AwaitHead {
        rx: oneshot::Receiver<hyper::Response<BoxBody<Bytes, ()>>>,
        http_version: hyper::Version,
        legacy: bool,
    },
    AwaitBody {
        parts: Parts,
        ready: VecDeque<InternalHttpBodyFrame>,
        body: BoxBody<Bytes, ()>,
        legacy: bool,
    },
}

impl Future for FullResponseFut {
    type Output = StreamEvent;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match this {
                Self::AwaitHead {
                    rx,
                    http_version,
                    legacy,
                } => match std::task::ready!(rx.poll_unpin(cx)) {
                    Ok(head) => {
                        let (parts, body) = head.into_parts();
                        *this = Self::AwaitBody {
                            parts,
                            ready: Default::default(),
                            body,
                            legacy: *legacy,
                        }
                    }
                    Err(..) => {
                        let mode = if *legacy {
                            ResponseMode::Legacy
                        } else {
                            ResponseMode::Framed
                        };
                        break Poll::Ready(StreamEvent::Response(mode.bad_gateway(*http_version)));
                    }
                },

                Self::AwaitBody {
                    parts,
                    ready,
                    body,
                    legacy,
                } => match std::task::ready!(Pin::new(body).poll_frame(cx)) {
                    Some(Ok(frame)) => {
                        ready.push_back(frame.into());
                    }
                    Some(Err(())) => {
                        let mode = if *legacy {
                            ResponseMode::Legacy
                        } else {
                            ResponseMode::Framed
                        };
                        break Poll::Ready(StreamEvent::Response(mode.bad_gateway(parts.version)));
                    }
                    None => {
                        let body = if *legacy {
                            let total_len = ready
                                .iter()
                                .map(|frame| match frame {
                                    InternalHttpBodyFrame::Data(data) => data.len(),
                                    InternalHttpBodyFrame::Trailers(..) => 0,
                                })
                                .sum();
                            let mut acc = BytesMut::with_capacity(total_len);
                            for frame in std::mem::take(ready) {
                                if let InternalHttpBodyFrame::Data(data) = frame {
                                    acc.extend_from_slice(data.as_ref());
                                }
                            }
                            InternalResponseBody::Legacy(Payload(acc.freeze()))
                        } else {
                            InternalResponseBody::Framed(InternalHttpBody(std::mem::take(ready)))
                        };
                        let response = InternalHttpResponse {
                            status: parts.status,
                            version: parts.version,
                            headers: std::mem::take(&mut parts.headers),
                            body,
                        };
                        break Poll::Ready(StreamEvent::Response(response));
                    }
                },
            }
        }
    }
}

impl fmt::Debug for FullResponseFut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitHead {
                rx,
                http_version,
                legacy,
            } => f
                .debug_struct("AwaitHead")
                .field("rx", rx)
                .field("http_version", http_version)
                .field("legacy", legacy)
                .finish(),
            Self::AwaitBody {
                parts,
                ready,
                body,
                legacy,
            } => f
                .debug_struct("AwaitBody")
                .field("uri", &parts.status)
                .field("http_version", &parts.version)
                .field("ready", &ready)
                .field("body_is_end_stream", &body.is_end_stream())
                .field("legacy", legacy)
                .finish(),
        }
    }
}

/// [`Stream`] that receives a [`hyper::Response`].
///
/// Yields the response and its body's chunks as [`InternalResponseBody::Chunked`].
enum ChunkedResponseStream {
    AwaitHead {
        rx: oneshot::Receiver<hyper::Response<BoxBody<Bytes, ()>>>,
        http_version: hyper::Version,
    },
    AwaitBody(BoxBody<Bytes, ()>),
    Finished,
}

impl ChunkedResponseStream {
    fn poll_chunk(
        body: &mut BoxBody<Bytes, ()>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<InternalHttpBodyNew, ()>> {
        let mut body = Pin::new(body);

        let first = std::task::ready!(body.as_mut().poll_frame(cx)).transpose()?;
        let Some(first) = first else {
            return Poll::Ready(Ok(InternalHttpBodyNew {
                frames: Default::default(),
                is_last: true,
            }));
        };

        let mut result = InternalHttpBodyNew {
            frames: vec![first.into()],
            is_last: false,
        };

        loop {
            match body.as_mut().poll_frame(cx) {
                Poll::Pending => break,
                Poll::Ready(None) => {
                    result.is_last = true;
                    break;
                }
                Poll::Ready(Some(Ok(frame))) => {
                    result.frames.push(frame.into());
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
            }
        }

        Poll::Ready(Ok(result))
    }
}

impl Stream for ChunkedResponseStream {
    type Item = StreamEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        match this {
            Self::AwaitHead { rx, http_version } => match std::task::ready!(rx.poll_unpin(cx)) {
                Ok(head) => {
                    let (parts, mut body) = head.into_parts();
                    let chunk = match Self::poll_chunk(&mut body, cx) {
                        Poll::Ready(Ok(chunk)) => chunk,
                        Poll::Pending => InternalHttpBodyNew {
                            frames: Default::default(),
                            is_last: false,
                        },
                        Poll::Ready(Err(())) => {
                            let response = ResponseMode::Chunked.bad_gateway(*http_version);
                            *this = Self::Finished;
                            return Poll::Ready(Some(StreamEvent::Response(response)));
                        }
                    };

                    *this = if chunk.is_last {
                        Self::Finished
                    } else {
                        Self::AwaitBody(body)
                    };

                    let response = InternalHttpResponse {
                        status: parts.status,
                        version: parts.version,
                        headers: parts.headers,
                        body: InternalResponseBody::Chunked(chunk),
                    };

                    Poll::Ready(Some(StreamEvent::Response(response)))
                }
                Err(..) => {
                    let response = ResponseMode::Chunked.bad_gateway(*http_version);
                    *this = Self::Finished;
                    Poll::Ready(Some(StreamEvent::Response(response)))
                }
            },

            Self::AwaitBody(body) => match std::task::ready!(Self::poll_chunk(body, cx)) {
                Ok(chunk) => {
                    if chunk.is_last {
                        *this = Self::Finished;
                    }
                    Poll::Ready(Some(StreamEvent::ResponseBodyChunk(chunk)))
                }
                Err(()) => {
                    *this = Self::Finished;
                    Poll::Ready(Some(StreamEvent::ResponseBodyError))
                }
            },

            Self::Finished => Poll::Ready(None),
        }
    }
}

impl fmt::Debug for ChunkedResponseStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitHead { rx, http_version } => f
                .debug_struct("AwaitHead")
                .field("rx", rx)
                .field("http_version", http_version)
                .finish(),
            Self::AwaitBody(body) => f
                .debug_struct("AwaitBody")
                .field("body_is_end_stream", &body.is_end_stream())
                .finish(),
            Self::Finished => f.write_str("Finished"),
        }
    }
}

/// [`Future`] that maps [`FifoClosed`] into [`StreamEvent::InboundDataClosed`].
#[derive(Debug)]
struct InboundDataClosedFut(FifoClosed<Bytes>);

impl Future for InboundDataClosedFut {
    type Output = StreamEvent;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut()
            .0
            .poll_unpin(cx)
            .map(|_| StreamEvent::InboundDataClosed)
    }
}

/// [`Stream`] that maps [`FifoStream`] items to [`StreamEvent::OutboundData`] and
/// [`StreamEvent::OutboundDataClosed`].
#[derive(Debug)]
struct OutboundDataStream(Option<FifoStream<Bytes>>);

impl Stream for OutboundDataStream {
    type Item = StreamEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(stream) = this.0.as_mut() else {
            return Poll::Ready(None);
        };
        loop {
            match std::task::ready!(stream.poll_next_unpin(cx)) {
                Some(data) if data.is_empty() => {}
                Some(data) => break Poll::Ready(Some(StreamEvent::OutboundData(data))),
                None => {
                    this.0 = None;
                    break Poll::Ready(Some(StreamEvent::OutboundDataClosed));
                }
            }
        }
    }
}

/// [`Future`] that maps [`FifoClosed`] into [`StreamEvent::InboundBodyClosed`].
#[derive(Debug)]
struct InboundBodyClosedFut(FifoClosed<Result<InternalHttpBodyNew, BodyError>>);

impl Future for InboundBodyClosedFut {
    type Output = StreamEvent;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut()
            .0
            .poll_unpin(cx)
            .map(|_| StreamEvent::InboundBodyClosed)
    }
}

/// Enum type for different HTTP response body types used in [`mirrord_protocol`] with
/// [`InternalHttpResponse`].
#[derive(EnumDiscriminants, Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[strum_discriminants(name(ResponseMode))]
pub enum InternalResponseBody {
    /// For [`LayerTcpSteal::HttpResponse`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponse)
    Legacy(Payload),
    /// For [`LayerTcpSteal::HttpResponseFramed`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponseFramed)
    Framed(InternalHttpBody),
    /// For [`LayerTcpSteal::HttpResponseChunked`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponseChunked)
    Chunked(InternalHttpBodyNew),
}

impl InternalResponseBody {
    /// Returns whether the response body is finished, meaning that there will be no more frames.
    pub fn is_finished(&self) -> bool {
        match self {
            Self::Legacy(..) | Self::Framed(..) => true,
            Self::Chunked(body) => body.is_last,
        }
    }
}

impl ResponseMode {
    const FALLBACK_BODY: Payload = Payload(Bytes::from_static(
        b"mirrord-client: response channel dropped",
    ));

    /// Returnes the most recent variant supported by the given [`mirrord_protocol`] version.
    pub fn most_recent(protocol_version: &semver::Version) -> Self {
        if HTTP_CHUNKED_RESPONSE_VERSION.matches(protocol_version) {
            Self::Chunked
        } else if HTTP_FRAMED_VERSION.matches(protocol_version) {
            Self::Framed
        } else {
            Self::Legacy
        }
    }

    /// Produces a [`StatusCode::BAD_GATEWAY`] response with the given HTTP version and correct body
    /// variant.
    pub fn bad_gateway(
        self,
        http_version: hyper::Version,
    ) -> InternalHttpResponse<InternalResponseBody> {
        match self {
            ResponseMode::Legacy => InternalHttpResponse {
                status: StatusCode::BAD_GATEWAY,
                version: http_version,
                headers: Default::default(),
                body: InternalResponseBody::Legacy(Self::FALLBACK_BODY.clone()),
            },

            ResponseMode::Framed => InternalHttpResponse {
                status: StatusCode::BAD_GATEWAY,
                version: http_version,
                headers: Default::default(),
                body: InternalResponseBody::Framed(InternalHttpBody(
                    vec![InternalHttpBodyFrame::Data(Self::FALLBACK_BODY.clone())].into(),
                )),
            },

            ResponseMode::Chunked => InternalHttpResponse {
                status: StatusCode::BAD_GATEWAY,
                version: http_version,
                headers: Default::default(),
                body: InternalResponseBody::Chunked(InternalHttpBodyNew {
                    frames: vec![InternalHttpBodyFrame::Data(Self::FALLBACK_BODY.clone())],
                    is_last: true,
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, time::Duration};

    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use http_body::Frame;
    use http_body_util::{StreamBody, combinators::BoxBody};
    use hyper::StatusCode;
    use mirrord_protocol::{
        Payload,
        tcp::{InternalHttpBody, InternalHttpBodyFrame, InternalHttpBodyNew, InternalHttpResponse},
    };
    use rstest::rstest;
    use tokio::sync::{mpsc, oneshot};
    use tokio_stream::wrappers::ReceiverStream;

    use crate::{
        client::tunnels::{
            TunnelId, TunnelType,
            streams::{
                ChunkedResponseStream, InternalResponseBody, InternalStreams, ResponseMode,
                StreamEvent,
            },
        },
        fifo::Fifo,
    };

    /// Verifies that [`InternalStreams::push_inbound_data_closed`] behaves as expected.
    #[tokio::test]
    async fn inbound_data_closed() {
        let mut streams = InternalStreams::default();
        let fifo = Fifo::with_capacity(NonZeroUsize::MAX);
        let id = TunnelId(TunnelType::OutgoingTcp, 0);
        let _abort = streams.push_inbound_data_closed(id, fifo.closed);
        drop(fifo.stream);
        assert_eq!(
            streams.next().await.unwrap(),
            (id, StreamEvent::InboundDataClosed),
        );
        assert!(streams.next().await.is_none());
    }

    /// Verifies that [`InternalStreams::push_inbound_body_closed`] behaves as expected.
    #[tokio::test]
    async fn inbound_body_closed() {
        let mut streams = InternalStreams::default();
        let fifo = Fifo::with_capacity(NonZeroUsize::MAX);
        let id = TunnelId(TunnelType::OutgoingTcp, 0);
        let _abort = streams.push_inbound_body_closed(id, fifo.closed);
        drop(fifo.stream);
        assert_eq!(
            streams.next().await.unwrap(),
            (id, StreamEvent::InboundBodyClosed),
        );
        assert!(streams.next().await.is_none());
    }

    /// Verifies that [`InternalStreams::push_outbound_data`] behaves as expected.
    #[tokio::test]
    async fn outbound_body() {
        let mut streams = InternalStreams::default();
        let mut fifo = Fifo::with_capacity(NonZeroUsize::MAX);
        let id = TunnelId(TunnelType::OutgoingTcp, 0);
        let _abort = streams.push_outbound_data(id, fifo.stream);

        fifo.sink.send(Bytes::from_static(b"hello")).await.unwrap();
        assert_eq!(
            streams.next().await.unwrap(),
            (id, StreamEvent::OutboundData(Bytes::from_static(b"hello"))),
        );

        fifo.sink.send(Bytes::from_static(b"there")).await.unwrap();
        assert_eq!(
            streams.next().await.unwrap(),
            (id, StreamEvent::OutboundData(Bytes::from_static(b"there"))),
        );

        drop(fifo.sink);
        assert_eq!(
            streams.next().await.unwrap(),
            (id, StreamEvent::OutboundDataClosed),
        );
        assert_eq!(streams.next().await, None);
    }

    /// Verifies that [`InternalStreams::push_response`] is able to read a response in
    /// [`ResponseMode::Legacy`] mode.
    #[tokio::test]
    async fn response_legacy() {
        let mut streams = InternalStreams::default();
        let id = TunnelId(TunnelType::IncomingSteal, 0);
        let (tx, rx) = oneshot::channel();
        let _abort = streams.push_response(id, rx, ResponseMode::Legacy, hyper::Version::HTTP_11);

        let (body_tx, body_rx) = mpsc::channel(8);
        let body = BoxBody::new(StreamBody::new(ReceiverStream::new(body_rx)));
        tx.send(hyper::Response::new(body)).unwrap();

        body_tx
            .send(Ok(Frame::data(Bytes::from_static(b"hello "))))
            .await
            .unwrap();
        body_tx
            .send(Ok(Frame::data(Bytes::from_static(b"there"))))
            .await
            .unwrap();
        body_tx
            .send(Ok(Frame::trailers(Default::default())))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_millis(100), streams.next())
            .await
            .unwrap_err();
        drop(body_tx);

        assert_eq!(
            streams.next().await.unwrap(),
            (
                id,
                StreamEvent::Response(InternalHttpResponse {
                    status: StatusCode::OK,
                    version: hyper::Version::HTTP_11,
                    headers: Default::default(),
                    body: InternalResponseBody::Legacy(Payload(Bytes::from_static(b"hello there"))),
                }),
            )
        );
        assert_eq!(streams.next().await, None);
    }

    /// Verifies that [`InternalStreams::push_response`] is able to read a response in
    /// [`ResponseMode::Framed`] mode.
    #[tokio::test]
    async fn response_framed() {
        let mut streams = InternalStreams::default();
        let id = TunnelId(TunnelType::IncomingSteal, 0);
        let (tx, rx) = oneshot::channel();
        let _abort = streams.push_response(id, rx, ResponseMode::Framed, hyper::Version::HTTP_11);

        let (body_tx, body_rx) = mpsc::channel(8);
        let body = BoxBody::new(StreamBody::new(ReceiverStream::new(body_rx)));
        tx.send(hyper::Response::new(body)).unwrap();

        body_tx
            .send(Ok(Frame::data(Bytes::from_static(b"hello "))))
            .await
            .unwrap();
        body_tx
            .send(Ok(Frame::data(Bytes::from_static(b"there"))))
            .await
            .unwrap();
        body_tx
            .send(Ok(Frame::trailers(Default::default())))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_millis(100), streams.next())
            .await
            .unwrap_err();
        drop(body_tx);

        assert_eq!(
            streams.next().await.unwrap(),
            (
                id,
                StreamEvent::Response(InternalHttpResponse {
                    status: StatusCode::OK,
                    version: hyper::Version::HTTP_11,
                    headers: Default::default(),
                    body: InternalResponseBody::Framed(InternalHttpBody(
                        vec![
                            InternalHttpBodyFrame::Data(Payload(Bytes::from_static(b"hello "))),
                            InternalHttpBodyFrame::Data(Payload(Bytes::from_static(b"there"))),
                            InternalHttpBodyFrame::Trailers(Default::default()),
                        ]
                        .into()
                    )),
                }),
            )
        );
        assert_eq!(streams.next().await, None);
    }

    /// Verifies that [`InternalStreams::push_response`] is able to read a response in
    /// [`ResponseMode::Chunked`] mode.
    #[tokio::test]
    async fn response_chunked() {
        let mut streams = InternalStreams::default();
        let id = TunnelId(TunnelType::IncomingSteal, 0);
        let (tx, rx) = oneshot::channel();
        let _abort = streams.push_response(id, rx, ResponseMode::Chunked, hyper::Version::HTTP_11);

        let (body_tx, body_rx) = mpsc::channel(8);
        let body = BoxBody::new(StreamBody::new(ReceiverStream::new(body_rx)));
        tx.send(hyper::Response::new(body)).unwrap();
        assert_eq!(
            streams.next().await.unwrap(),
            (
                id,
                StreamEvent::Response(InternalHttpResponse {
                    status: StatusCode::OK,
                    version: hyper::Version::HTTP_11,
                    headers: Default::default(),
                    body: InternalResponseBody::Chunked(InternalHttpBodyNew {
                        frames: Default::default(),
                        is_last: false,
                    }),
                }),
            )
        );

        let frames = [
            InternalHttpBodyFrame::Data(Payload(Bytes::from_static(b"hello "))),
            InternalHttpBodyFrame::Data(Payload(Bytes::from_static(b"there"))),
            InternalHttpBodyFrame::Trailers(Default::default()),
        ];
        for frame in frames {
            body_tx.send(Ok(frame.clone().into())).await.unwrap();
            assert_eq!(
                streams.next().await.unwrap(),
                (
                    id,
                    StreamEvent::ResponseBodyChunk(InternalHttpBodyNew {
                        frames: vec![frame],
                        is_last: false,
                    }),
                )
            );
        }

        drop(body_tx);
        assert_eq!(
            streams.next().await.unwrap(),
            (
                id,
                StreamEvent::ResponseBodyChunk(InternalHttpBodyNew {
                    frames: Default::default(),
                    is_last: true
                })
            )
        );

        assert_eq!(streams.next().await, None);
    }

    /// Verifies that [`InternalStreams::push_response`] correctly handles later body read errors
    /// in [`ResponseMode::Chunked`] mode.
    #[tokio::test]
    async fn response_chunked_body_error() {
        let mut streams = InternalStreams::default();
        let id = TunnelId(TunnelType::IncomingSteal, 0);
        let (tx, rx) = oneshot::channel();
        let _abort = streams.push_response(id, rx, ResponseMode::Chunked, hyper::Version::HTTP_11);

        let (body_tx, body_rx) = mpsc::channel(8);
        let body = BoxBody::new(StreamBody::new(ReceiverStream::new(body_rx)));
        tx.send(hyper::Response::new(body)).unwrap();
        assert_eq!(
            streams.next().await.unwrap(),
            (
                id,
                StreamEvent::Response(InternalHttpResponse {
                    status: StatusCode::OK,
                    version: hyper::Version::HTTP_11,
                    headers: Default::default(),
                    body: InternalResponseBody::Chunked(InternalHttpBodyNew {
                        frames: Default::default(),
                        is_last: false,
                    }),
                }),
            )
        );

        body_tx.send(Err(())).await.unwrap();
        assert_eq!(
            streams.next().await.unwrap(),
            (id, StreamEvent::ResponseBodyError,)
        );
        assert_eq!(streams.next().await, None);
    }

    /// Verifies that [`InternalStreams::push_response`] correctly handles early response errors.
    #[rstest]
    #[case(
        ResponseMode::Legacy,
        InternalResponseBody::Legacy(ResponseMode::FALLBACK_BODY.clone()),
    )]
    #[case(
        ResponseMode::Framed,
        InternalResponseBody::Framed(
            InternalHttpBody(vec![InternalHttpBodyFrame::Data(ResponseMode::FALLBACK_BODY.clone())].into())
        ),
    )]
    #[case(
        ResponseMode::Chunked,
        InternalResponseBody::Chunked(InternalHttpBodyNew {
            frames: vec![InternalHttpBodyFrame::Data(ResponseMode::FALLBACK_BODY.clone())],
            is_last: true,
        }),
    )]
    #[tokio::test]
    async fn response_dropped(
        #[case] mode: ResponseMode,
        #[case] expected_body: InternalResponseBody,
        #[values(true, false)] oneshot_dropped: bool,
    ) {
        let mut streams = InternalStreams::default();
        let id = TunnelId(TunnelType::IncomingSteal, 0);
        let (tx, rx) = oneshot::channel();
        let _abort = streams.push_response(id, rx, mode, hyper::Version::HTTP_11);

        if oneshot_dropped {
            drop(tx);
        } else {
            let body = BoxBody::new(StreamBody::new(futures::stream::iter([Err(())])));
            let response = hyper::Response::new(body);
            tx.send(response).unwrap();
        }

        assert_eq!(
            streams.next().await.unwrap(),
            (
                id,
                StreamEvent::Response(InternalHttpResponse {
                    status: StatusCode::BAD_GATEWAY,
                    version: hyper::Version::HTTP_11,
                    headers: Default::default(),
                    body: expected_body,
                }),
            ),
        );
        assert_eq!(streams.next().await, None);
    }

    /// Verifies that [`InternalStreams`] streams can be aborted,
    /// and aborted streams do not produce any more messages.
    #[tokio::test]
    async fn abort() {
        let mut streams = InternalStreams::default();

        let inbound_data_fifo = Fifo::with_capacity(NonZeroUsize::MAX);
        let abort_1 = streams.push_inbound_data_closed(
            TunnelId(TunnelType::IncomingSteal, 0),
            inbound_data_fifo.closed,
        );

        let inbound_body_fifo = Fifo::with_capacity(NonZeroUsize::MAX);
        let abort_2 = streams.push_inbound_body_closed(
            TunnelId(TunnelType::IncomingSteal, 1),
            inbound_body_fifo.closed,
        );

        let outbound_data_fifo = Fifo::with_capacity(NonZeroUsize::MAX);
        let abort_3 = streams.push_outbound_data(
            TunnelId(TunnelType::IncomingSteal, 2),
            outbound_data_fifo.stream,
        );

        let (_response_tx, response_rx) = oneshot::channel();
        let abort_4 = streams.push_response(
            TunnelId(TunnelType::IncomingSteal, 3),
            response_rx,
            ResponseMode::Chunked,
            hyper::Version::HTTP_11,
        );

        tokio::time::timeout(Duration::from_millis(100), streams.next())
            .await
            .unwrap_err();

        drop(abort_1);
        drop(abort_2);
        drop(abort_3);
        drop(abort_4);

        assert_eq!(streams.next().await, None,);
    }

    /// Verifies that [`ChunkedResponseStream::poll_chunk`] correctly
    /// transforms body stream into chunks.
    #[tokio::test]
    async fn poll_chunk() {
        const DATA: Bytes = Bytes::from_static(b"hello");

        let mut fifo = Fifo::<Bytes>::with_capacity(NonZeroUsize::MAX);
        let mut body = BoxBody::new(StreamBody::new(fifo.stream.map(Frame::data).map(Ok)));

        fifo.sink.send(DATA.clone()).await.unwrap();

        let chunk = futures::future::poll_fn(|cx| ChunkedResponseStream::poll_chunk(&mut body, cx))
            .await
            .unwrap();
        assert_eq!(
            chunk,
            InternalHttpBodyNew {
                frames: vec![InternalHttpBodyFrame::Data(Payload(DATA.clone()))],
                is_last: false,
            },
        );

        fifo.sink.send(DATA.clone()).await.unwrap();
        fifo.sink.send(DATA.clone()).await.unwrap();
        let chunk = futures::future::poll_fn(|cx| ChunkedResponseStream::poll_chunk(&mut body, cx))
            .await
            .unwrap();
        assert_eq!(
            chunk,
            InternalHttpBodyNew {
                frames: vec![
                    InternalHttpBodyFrame::Data(Payload(DATA.clone())),
                    InternalHttpBodyFrame::Data(Payload(DATA.clone())),
                ],
                is_last: false,
            },
        );

        fifo.sink.send(DATA.clone()).await.unwrap();
        fifo.sink.send(DATA.clone()).await.unwrap();
        fifo.sink.close().await.unwrap();
        let chunk = futures::future::poll_fn(|cx| ChunkedResponseStream::poll_chunk(&mut body, cx))
            .await
            .unwrap();
        assert_eq!(
            chunk,
            InternalHttpBodyNew {
                frames: vec![
                    InternalHttpBodyFrame::Data(Payload(DATA.clone())),
                    InternalHttpBodyFrame::Data(Payload(DATA.clone())),
                ],
                is_last: true,
            },
        );
    }
}
