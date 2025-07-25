use std::{
    error::Report,
    fmt,
    future::Future,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{future::Either, FutureExt, Stream};
use http_body_util::combinators::BoxBody;
use hyper::{
    body::{Body, Frame, Incoming, SizeHint},
    http::request::Parts,
    server::conn::{http1, http2},
    service::Service,
    upgrade::OnUpgrade,
    Error, Request, Response,
};
use hyper_util::rt::TokioExecutor;
use mirrord_protocol::batched_body::{BatchedBody, Frames};
use tokio::sync::{mpsc, oneshot};

use super::{error::MirrordErrorResponse, BoxResponse, HttpVersion};
use crate::metrics::{MetricGuard, REDIRECTED_REQUESTS};

/// An HTTP request extracted from an HTTP connection
/// with [`ExtractedRequests`].
pub struct ExtractedRequest {
    /// Parts of the request.
    pub parts: Parts,
    /// First frames of the request body.
    pub body_head: Vec<Frame<Bytes>>,
    /// Rest of the request body frames (if any).
    pub body_tail: Option<Incoming>,
    /// An HTTP upgrade extracted from the request.
    pub upgrade: OnUpgrade,
    /// Channel for sending the response back to the HTTP client.
    ///
    /// Try not to drop it without providing a meaningful error response
    /// ([`MirrordErrorResponse`]).
    /// Providing meaningful error responses helps when debugging user issues.
    pub response_tx: oneshot::Sender<BoxResponse>,
}

impl fmt::Debug for ExtractedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractedRequest")
            .field("parts", &self.parts)
            .field("body_head", &self.body_head)
            .field("has_more_body", &self.body_tail.is_some())
            .finish()
    }
}

/// A [`Stream`] of HTTP requests extracted from an HTTP connection.
///
/// **Important:** this stream has to be polled with [`Stream::poll_next`] for the underlying HTTP
/// connection to make any progress.
///
/// # Metrics
///
/// This type handles managing the [`REDIRECTED_REQUESTS`] metric,
/// you don't need to do it manually.
///
/// The metric is incremented when a new request is extracted, and decremented when hyper finishes
/// processing the response.
pub struct ExtractedRequests<IO> {
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
    type Item = hyper::Result<ExtractedRequest>;

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
                    upgrade,
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
    type Response = Response<MetricGuardedBody>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let this = self.clone();

        async move {
            let metric_guard = MetricGuard::new(&REDIRECTED_REQUESTS);

            let (response_tx, response_rx) = oneshot::channel();
            let version = request.version();

            if this.request_tx.send((request, response_tx)).await.is_err() {
                let response = BoxResponse::from(MirrordErrorResponse::new(
                    version,
                    "the connection was dropped",
                ));
                let response = response.map(|body| MetricGuardedBody {
                    body,
                    _metric_guard: metric_guard,
                });
                return Ok(response);
            }

            let response = match response_rx.await {
                Ok(response) => response,
                Err(..) => MirrordErrorResponse::new(version, "the request was dropped").into(),
            };

            let response = response.map(|body| MetricGuardedBody {
                body,
                _metric_guard: metric_guard,
            });

            Ok(response)
        }
        .boxed()
    }
}

/// Used for responses returned from [`InnerService`].
///
/// Holds a [`MetricGuard`], so the [`REDIRECTED_REQUESTS`] metric stays bumped until
/// hyper finishes processing the response.
struct MetricGuardedBody {
    body: BoxBody<Bytes, hyper::Error>,
    _metric_guard: MetricGuard,
}

impl Body for MetricGuardedBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        Pin::new(&mut this.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use bytes::Bytes;
    use futures::StreamExt;
    use http_body_util::{BodyExt, Empty};
    use hyper::{http::StatusCode, Request, Response};
    use hyper_util::rt::TokioIo;
    use rstest::rstest;
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::Notify,
    };

    use crate::http::{extract_requests::ExtractedRequests, sender::HttpSender, HttpVersion};

    /// Verifies that [`ExtractedRequests`] works correctly
    /// and can be gracefully shut down.
    #[rstest]
    #[tokio::test]
    async fn basic_extract_requests(
        #[values(HttpVersion::V1, HttpVersion::V2)] version: HttpVersion,
    ) {
        let notify = Arc::new(Notify::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        let addr = listener.local_addr().unwrap();
        let notify_cloned = notify.clone();
        let client = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let mut sender = HttpSender::new(TokioIo::new(stream), version)
                .await
                .unwrap();
            for _ in 0..2 {
                let response = sender
                    .send(Request::new(Empty::<Bytes>::new()))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body = response.into_body().collect().await.unwrap().to_bytes();
                assert!(body.is_empty());
            }

            notify_cloned.notified().await;
            sender
                .send(Request::new(Empty::<Bytes>::new()))
                .await
                .unwrap_err();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let mut requests = ExtractedRequests::new(TokioIo::new(stream), version);

        let request = requests.next().await.unwrap().unwrap();
        let _ = request.response_tx.send(Response::new(
            Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed(),
        ));

        let request = requests.next().await.unwrap().unwrap();
        let _ = request.response_tx.send(Response::new(
            Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed(),
        ));

        requests.graceful_shutdown();
        notify.notify_one();

        let request = requests.next().await;
        assert!(request.is_none());

        client.await.unwrap();
    }

    /// Verifies that [`ExtractedRequests`] automatically provide a 502 response
    /// when the response sender is dropped.
    #[rstest]
    #[tokio::test]
    async fn extract_requests_dropped_response(
        #[values(HttpVersion::V1, HttpVersion::V2)] version: HttpVersion,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let mut sender = HttpSender::new(TokioIo::new(stream), version)
                .await
                .unwrap();
            let response = sender
                .send(Request::new(Empty::<Bytes>::new()))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        });

        let (stream, _) = listener.accept().await.unwrap();
        let mut requests = ExtractedRequests::new(TokioIo::new(stream), version);

        let request = requests.next().await.unwrap().unwrap();
        std::mem::drop(request);

        let request = requests.next().await;
        assert!(request.is_none());

        client.await.unwrap();
    }
}
