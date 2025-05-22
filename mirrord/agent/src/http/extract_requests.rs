use std::{
    error::Report,
    fmt,
    future::Future,
    marker::PhantomData,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{future::Either, FutureExt, Stream};
use hyper::{
    body::{Frame, Incoming},
    http::request::Parts,
    server::conn::{http1, http2},
    service::Service,
    upgrade::OnUpgrade,
    Error, Request,
};
use hyper_util::rt::TokioExecutor;
use mirrord_protocol::batched_body::{BatchedBody, Frames};
use tokio::sync::{mpsc, oneshot};

use super::{error::MirrordErrorResponse, BoxResponse, HttpVersion};

/// An HTTP request extracted from an HTTP connection
/// with [`ExtractedRequests`].
pub struct ExtractedRequest<IO> {
    /// Parts of the request.
    pub parts: Parts,
    /// First frames of the request body.
    pub body_head: Vec<Frame<Bytes>>,
    /// Rest of the request body frames (if any).
    pub body_tail: Option<Incoming>,
    /// An HTTP upgrade extracted from the request.
    pub upgrade: HttpUpgrade<IO>,
    /// Channel for sending the response back to the HTTP client.
    ///
    /// Try not to drop it without providing a meaningful error response
    /// ([`MirrordErrorResponse`]).
    /// Providing meaningful error responses helps when debugging user issues.
    pub response_tx: oneshot::Sender<BoxResponse>,
}

impl<IO> fmt::Debug for ExtractedRequest<IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractedRequest")
            .field("parts", &self.parts)
            .field("body_head", &self.body_head)
            .field("has_more_body", &self.body_tail.is_some())
            .finish()
    }
}

/// Wrapper over [`OnUpgrade`], that provides type safe downcasting.
///
/// Implements [`Future`].
pub struct HttpUpgrade<IO> {
    pub upgrade: OnUpgrade,
    _io_type: PhantomData<fn() -> IO>,
}

impl<IO> Future for HttpUpgrade<IO>
where
    IO: 'static + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    type Output = hyper::Result<(IO, Bytes)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let upgraded = std::task::ready!(Pin::new(&mut this.upgrade).poll(cx));
        match upgraded {
            Ok(upgrade) => {
                let parts = upgrade.downcast::<IO>().expect("io type is known");
                Poll::Ready(Ok((parts.io, parts.read_buf)))
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

/// A [`Stream`] of HTTP requests extracted from an HTTP connection.
pub struct ExtractedRequests<IO>
where
    IO: 'static,
{
    request_rx: mpsc::Receiver<(Request<Incoming>, oneshot::Sender<BoxResponse>)>,
    connection: Option<Either<ConnV1<IO>, ConnV2<IO>>>,
}

impl<IO> ExtractedRequests<IO>
where
    IO: 'static + hyper::rt::Read + hyper::rt::Write + Unpin + Send,
{
    pub fn new(conn: IO, version: HttpVersion) -> Self {
        let (request_tx, request_rx) = mpsc::channel(4);
        let service = InnerService { request_tx };

        let connection = match version {
            HttpVersion::V1 => {
                let conn = http1::Builder::new()
                    .preserve_header_case(true)
                    .serve_connection(conn, service)
                    .with_upgrades();
                Either::Left(conn)
            }

            HttpVersion::V2 => {
                let conn =
                    http2::Builder::new(TokioExecutor::default()).serve_connection(conn, service);
                Either::Right(conn)
            }
        };

        Self {
            request_rx,
            connection: Some(connection),
        }
    }

    pub fn graceful_shutdown(&mut self) {
        match &mut self.connection {
            Some(Either::Left(conn)) => Pin::new(conn).graceful_shutdown(),
            Some(Either::Right(conn)) => Pin::new(conn).graceful_shutdown(),
            None => {}
        }
    }
}

impl<IO> Stream for ExtractedRequests<IO>
where
    IO: 'static + hyper::rt::Read + hyper::rt::Write + Unpin + Send,
{
    type Item = hyper::Result<ExtractedRequest<IO>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            if let Poll::Ready(Some((mut request, response_tx))) = this.request_rx.poll_recv(cx) {
                let upgrade = hyper::upgrade::on(&mut request);
                let (parts, mut body) = request.into_parts();
                let Frames { frames, is_last } = match body.ready_frames() {
                    Ok(frames) => frames,
                    Err(error) => {
                        let _ = response_tx.send(
                            MirrordErrorResponse::new(parts.version, Report::new(error)).into(),
                        );
                        continue;
                    }
                };

                break Poll::Ready(Some(Ok(ExtractedRequest {
                    parts,
                    body_head: frames,
                    body_tail: is_last.not().then_some(body),
                    upgrade: HttpUpgrade {
                        upgrade,
                        _io_type: Default::default(),
                    },
                    response_tx,
                })));
            }

            let Some(connection) = this.connection.as_mut() else {
                break Poll::Ready(None);
            };

            let result = std::task::ready!(connection.poll_unpin(cx)).err().map(Err);
            this.connection = None;
            break Poll::Ready(result);
        }
    }
}

type ConnV1<IO> = http1::UpgradeableConnection<IO, InnerService>;
type ConnV2<IO> = http2::Connection<IO, InnerService, TokioExecutor>;

/// Implementation of [`Service`] that sends [`Request`]s through the inner [`mpsc::Sender`]
/// and returns responses produced somewhere else.
///
/// The responses are received from [`oneshot::Receiver`] send with the [`Request`].
///
/// Used internally by [`ExtractedRequests`].
#[derive(Clone)]
struct InnerService {
    request_tx: mpsc::Sender<(Request<Incoming>, oneshot::Sender<BoxResponse>)>,
}

impl Service<Request<Incoming>> for InnerService {
    type Response = BoxResponse;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let this = self.clone();

        async move {
            let (response_tx, response_rx) = oneshot::channel();
            let version = request.version();

            if this.request_tx.send((request, response_tx)).await.is_err() {
                return Ok(MirrordErrorResponse::new(version, "the connection was dropped").into());
            }

            let response = match response_rx.await {
                Ok(response) => response,
                Err(..) => MirrordErrorResponse::new(version, "the request was dropped").into(),
            };

            Ok(response)
        }
        .boxed()
    }
}
