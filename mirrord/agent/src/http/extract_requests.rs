use core::fmt;
use std::{
    future::Future,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{future::Either, FutureExt, Stream};
use http::request::Parts;
use http_body_util::combinators::BoxBody;
use hyper::{
    body::{Frame, Incoming},
    server::conn::{http1, http2},
    service::Service,
    upgrade::OnUpgrade,
    Error, Request, Response,
};
use hyper_util::rt::TokioExecutor;
use mirrord_protocol::batched_body::{BatchedBody, Frames};
use tokio::sync::{mpsc, oneshot};

use super::{error::MirrordErrorResponse, HttpVersion};

/// [`Response`] type used by [`ExtractedRequest`].
pub type BoxResponse = Response<BoxBody<Bytes, hyper::Error>>;

/// An HTTP request extracted from a redirected incoming connection
/// with [`ExtractedRequests`].
pub struct ExtractedRequest {
    pub parts: Parts,
    pub body_head: Vec<Frame<Bytes>>,
    pub body_tail: Option<Incoming>,
    pub on_upgrade: OnUpgrade,
    /// Can be used send the response back to the remote HTTP client.
    ///
    /// Try not to drop it without providing a meaningful error response
    /// ([`MirrordErrorResponse`]).
    /// Providing meaningful error responses helps when debugging user issues.
    pub response_tx: oneshot::Sender<BoxResponse>,
}

impl fmt::Debug for ExtractedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractedRequest")
            .field("parts", &self.parts)
            .field("ready_frames", &self.body_head.len())
            .field("body_finished", &self.body_tail.is_none())
            .finish()
    }
}

pub struct ExtractedRequests<IO> {
    request_rx: mpsc::Receiver<ExtractedRequest>,
    connection: Option<
        Either<
            http1::UpgradeableConnection<IO, InnerService>,
            http2::Connection<IO, InnerService, TokioExecutor>,
        >,
    >,
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
    type Item = hyper::Result<ExtractedRequest>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if let Poll::Ready(Some(request)) = this.request_rx.poll_recv(cx) {
            return Poll::Ready(Some(Ok(request)));
        }

        let Some(connection) = this.connection.as_mut() else {
            return Poll::Ready(None);
        };

        let result = std::task::ready!(connection.poll_unpin(cx)).err().map(Err);
        this.connection = None;
        Poll::Ready(result)
    }
}

/// Implementation of [`Service`] that sends [`Request`]s through the inner [`mpsc::Sender`]
/// and returns responses produced somewhere else.
///
/// The responses are received from [`oneshot::Receiver`] send with the [`Request`].
///
/// Used internally by [`ExtractedRequests`].
#[derive(Clone)]
struct InnerService {
    request_tx: mpsc::Sender<ExtractedRequest>,
}

impl Service<Request<Incoming>> for InnerService {
    type Response = BoxResponse;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        let this = self.clone();

        async move {
            let (response_tx, response_rx) = oneshot::channel();
            let version = request.version();
            let on_upgrade = hyper::upgrade::on(&mut request);
            let (parts, mut body) = request.into_parts();
            let Frames { frames, is_last } = body.ready_frames()?;
            let body_head = frames;
            let body_tail = is_last.not().then_some(body);

            let message = ExtractedRequest {
                parts,
                body_head,
                body_tail,
                on_upgrade,
                response_tx,
            };

            if this.request_tx.send(message).await.is_err() {
                return Ok(MirrordErrorResponse::new(
                    version,
                    "redirector task is exiting, request dropped",
                )
                .into());
            }

            let response = match response_rx.await {
                Ok(response) => response,
                Err(..) => MirrordErrorResponse::new(version, "request dropped").into(),
            };

            Ok(response)
        }
        .boxed()
    }
}
