//! Tests for redirecting HTTP requests with the
//! [`RedirectorTask`](crate::incoming::RedirectorTask).

use std::{
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    time::Duration,
};

use bytes::Bytes;
use futures::{
    future::{BoxFuture, Either},
    StreamExt,
};
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming,
    http::{Method, StatusCode, Version},
    service::Service,
    Request, Response,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use rstest::rstest;
use tokio::{net::TcpListener, task::JoinSet};
use tokio_util::sync::CancellationToken;

use crate::{
    http::HttpVersion,
    incoming::{
        test::dummy_redirector::DummyRedirector, BoxBody, IncomingStreamItem,
        IncomingTlsHandlerStore, RedirectorTask,
    },
};

pub async fn run_http_server<S>(
    listener: TcpListener,
    http_version: HttpVersion,
    service: S,
    shutdown: CancellationToken,
) where
    S: 'static
        + Clone
        + Service<Request<Incoming>, Response = Response<BoxBody>, Error = hyper::Error>
        + Send,
    S::Future: Send,
{
    let mut connection_tasks = JoinSet::new();

    loop {
        let conn = tokio::select! {
            result = listener.accept() => result.unwrap().0,
            _ = shutdown.cancelled() => {
                break;
            }
        };

        let mut conn = match http_version {
            HttpVersion::V1 => {
                let conn = hyper::server::conn::http1::Builder::new()
                    .preserve_header_case(true)
                    .serve_connection(TokioIo::new(conn), service.clone())
                    .with_upgrades();
                Either::Left(conn)
            }

            HttpVersion::V2 => {
                let conn = hyper::server::conn::http2::Builder::new(TokioExecutor::default())
                    .serve_connection(TokioIo::new(conn), service.clone());
                Either::Right(conn)
            }
        };

        let shutdown = shutdown.clone();
        connection_tasks.spawn(async move {
            tokio::select! {
                result = &mut conn => {
                    result.unwrap();
                    return;
                }

                _ = shutdown.cancelled() => {
                    match &mut conn {
                        Either::Left(conn) => {
                            Pin::new(conn).graceful_shutdown();
                        }
                        Either::Right(conn) => {
                            Pin::new(conn).graceful_shutdown();
                        }
                    }
                }
            }

            conn.await.unwrap();
        });
    }

    connection_tasks.join_all().await;
}

#[derive(Clone)]
pub struct ReadBodyReturnOk {
    pub expected_body: Vec<u8>,
}

impl ReadBodyReturnOk {
    pub const RESPONSE_BODY: &'static [u8] = b"remote";
}

impl Service<Request<Incoming>> for ReadBodyReturnOk {
    type Response = Response<BoxBody>;
    type Error = hyper::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let expected_body = self.expected_body.clone();

        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let collected = body.collect().await?.to_bytes();

            let mut response = Response::new(
                Full::new(Bytes::from_static(Self::RESPONSE_BODY))
                    .map_err(|_| unreachable!())
                    .boxed(),
            );
            *response.status_mut() = if collected == expected_body {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            };
            *response.version_mut() = parts.version;

            Ok(response)
        })
    }
}

/// Verifies full HTTP flow on multiple HTTP request and various subscription combinations.
///
/// All requests should be processed by all clients.
#[rstest]
#[case::steal_and_mirror(true, true)]
#[case::steal(true, false)]
#[case::mirror(false, true)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn http_full_flow(
    #[case] steal: bool,
    #[case] mirror: bool,
    #[values(HttpVersion::V1, HttpVersion::V2)] http_version: HttpVersion,
) {
    assert!(steal || mirror, "this test cannot handle no subscription");

    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap();
    let destination = listener.local_addr().unwrap();

    let (redirector, _, connections) = DummyRedirector::new();
    let (task, mut steal_handle, mut mirror_handle) =
        RedirectorTask::new(redirector, IncomingTlsHandlerStore::dummy());
    tokio::spawn(task.run());

    if steal {
        steal_handle.steal(destination.port()).await.unwrap();
    }
    if mirror {
        mirror_handle.mirror(destination.port()).await.unwrap();
    }

    let request_body = b"hello there, I am an HTTP request body";
    let token = CancellationToken::new();

    let mut tasks = JoinSet::new();
    if steal {
        tasks.spawn(async move {
            for _ in 0..2 {
                let mut request = steal_handle
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_http()
                    .steal();
                assert_eq!(
                    &request.request_head.body,
                    &[InternalHttpBodyFrame::Data(request_body.into())],
                );
                assert!(request.request_head.has_more_frames.not());
                request.response_provider.send(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(
                            Full::new(Bytes::from_static(b"stolen"))
                                .map_err(|_| unreachable!())
                                .boxed(),
                        )
                        .unwrap(),
                );
                let item = request.stream.next().await.unwrap();
                assert!(
                    matches!(&item, IncomingStreamItem::Finished(Ok(()))),
                    "{item:?}"
                );
            }
        });
    } else {
        tasks.spawn(run_http_server(
            listener,
            http_version,
            ReadBodyReturnOk {
                expected_body: request_body.to_vec(),
            },
            token.clone(),
        ));
    }

    if mirror {
        tasks.spawn(async move {
            for _ in 0..2 {
                let mut request = mirror_handle.next().await.unwrap().unwrap().unwrap_http();
                assert_eq!(
                    &request.request_head.body,
                    &[InternalHttpBodyFrame::Data(request_body.into())],
                );
                assert!(request.request_head.has_more_frames.not());
                let item = request.stream.next().await.unwrap();
                assert!(
                    matches!(&item, IncomingStreamItem::Finished(Ok(()))),
                    "{item:?}"
                );
            }
        });
    }

    let request = Request::builder()
        .method(Method::GET)
        .uri("http://some-server.com")
        .version(if http_version == HttpVersion::V1 {
            Version::HTTP_11
        } else {
            Version::HTTP_2
        })
        .body(Full::new(Bytes::from(request_body.as_slice())).map_err(|_| unreachable!()))
        .unwrap();
    let mut sender = connections.new_http(destination, http_version).await;
    let response = sender
        .send(request.clone().map(BoxBody::new))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let got_body = response.into_body().collect().await.unwrap().to_bytes();
    if steal {
        assert_eq!(got_body.as_ref(), b"stolen");
    } else {
        assert_eq!(got_body.as_ref(), ReadBodyReturnOk::RESPONSE_BODY);
    }

    let response = sender.send(request.map(BoxBody::new)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let got_body = response.into_body().collect().await.unwrap().to_bytes();
    if steal {
        assert_eq!(got_body.as_ref(), b"stolen");
    } else {
        assert_eq!(got_body.as_ref(), ReadBodyReturnOk::RESPONSE_BODY);
    }

    token.cancel();

    tasks.join_all().await;
}
