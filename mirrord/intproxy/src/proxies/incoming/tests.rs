use std::{convert::Infallible, time::Duration};

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::{BodyExt, Empty, StreamBody, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode, Version,
    body::{Frame, Incoming},
    service::{Service, service_fn},
};
use hyper_util::rt::TokioIo;
use mirrord_intproxy_protocol::{
    IncomingRequest, IncomingResponse, LayerId, PortSubscribe, PortSubscription,
    ProxyToLayerMessage,
};
use mirrord_protocol::{
    ClientMessage,
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, ChunkedResponse, DaemonTcp,
        HttpFilter, HttpMethodFilter, HttpRequestMetadata, IncomingTrafficTransportType,
        InternalHttpBodyFrame, InternalHttpBodyNew, InternalHttpRequest, LayerTcpSteal, StealType,
        TcpClose,
    },
};
use mirrord_protocol_io::Connection;
use rstest::rstest;
use tokio::net::TcpListener;

use crate::{
    background_tasks::BackgroundTasks,
    main_tasks::{ProxyMessage, ToLayer},
    proxies::incoming::{IncomingProxy, IncomingProxyError, IncomingProxyMessage},
    session_monitor::{MonitorEvent, MonitorTx},
};

/// Dummy service mocking a server sent events HTTP server.
///
/// Responds with an endless stream of `hello there\n` body frames.
struct SseService;

impl Service<Request<Incoming>> for SseService {
    type Error = hyper::Error;
    type Response = Response<BoxBody<Bytes, hyper::Error>>;
    type Future = std::future::Ready<hyper::Result<Response<BoxBody<Bytes, hyper::Error>>>>;

    fn call(&self, _req: Request<Incoming>) -> Self::Future {
        let body = futures::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some((Ok(Frame::data(Bytes::from_static(b"hello there\n"))), ()))
        });
        std::future::ready(Ok(Response::new(BoxBody::new(StreamBody::new(body)))))
    }
}

/// Verifies that [`IncomingProxy`] terminates in progress stolen HTTP requests,
/// when the remote counterpart was closed by the client.
#[rstest]
#[case::unfiltered(StealType::All(80))]
#[case::filtered(StealType::FilteredHttpEx(80, HttpFilter::Method(HttpMethodFilter::Get)))]
#[tokio::test]
async fn http_request_terminates_on_remote_close(#[case] steal_type: StealType) {
    let local_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local_listener.local_addr().unwrap();

    let (conn, _, out) = Connection::dummy();
    let proxy = IncomingProxy::new(
        Duration::from_secs(3),
        Default::default(),
        crate::session_monitor::MonitorTx::disabled(),
    );
    let mut background_tasks: BackgroundTasks<(), ProxyMessage, IncomingProxyError> =
        BackgroundTasks::new(conn.tx_handle());

    let proxy = background_tasks.register(proxy, (), 8);

    proxy
        .send(IncomingProxyMessage::AgentProtocolVersion(
            mirrord_protocol::VERSION.clone(),
        ))
        .await;

    // Make port subscription.
    proxy
        .send(IncomingProxyMessage::LayerRequest(
            0,
            LayerId(0),
            IncomingRequest::PortSubscribe(PortSubscribe {
                listening_on: local_addr.into(),
                subscription: PortSubscription::Steal(steal_type.clone()),
            }),
        ))
        .await;
    assert_eq!(
        out.next().await.unwrap(),
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(steal_type)),
    );
    proxy
        .send(IncomingProxyMessage::AgentSteal(
            DaemonTcp::SubscribeResult(Ok(80)),
        ))
        .await;
    assert_eq!(
        background_tasks.next().await.unwrap().1.unwrap_message(),
        ProxyMessage::ToLayer(ToLayer {
            message_id: 0,
            layer_id: LayerId(0),
            message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(())))
        }),
    );

    // Start local server sent events server.
    let local_server_task = tokio::spawn(async move {
        let (conn, _) = local_listener.accept().await.unwrap();
        let conn = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(conn), SseService);
        tokio::select! {
            _ = local_listener.accept() => panic!("expected only one connection"),
            res = conn => if res.is_ok() {
                panic!("connection should fail");
            }
        }
    });

    // Start the request.
    proxy
        .send(IncomingProxyMessage::AgentSteal(
            DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                connection_id: 0,
                request_id: 0,
                metadata: HttpRequestMetadata::V1 {
                    source: "127.0.0.1:55555".parse().unwrap(),
                    destination: "127.0.0.1:80".parse().unwrap(),
                },
                transport: IncomingTrafficTransportType::Tcp,
                request: InternalHttpRequest {
                    method: Method::GET,
                    uri: "http://127.0.0.1:80/hello/there".parse().unwrap(),
                    version: Version::HTTP_11,
                    headers: Default::default(),
                    body: InternalHttpBodyNew {
                        frames: Default::default(),
                        is_last: false,
                    },
                },
            })),
        ))
        .await;

    // In the background, consume HTTP response messages produced by the intproxy.
    // This ensures that the intproxy does not choke.
    let receive_frames = tokio::spawn(async move {
        let message = out.next().await.unwrap();
        match message {
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                ChunkedResponse::Start(response),
            )) => {
                assert_eq!(response.connection_id, 0);
                assert_eq!(response.request_id, 0);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        loop {
            assert_eq!(
                out.next().await.unwrap(),
                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                    ChunkedRequestBodyV1 {
                        is_last: false,
                        connection_id: 0,
                        request_id: 0,
                        frames: vec![InternalHttpBodyFrame::Data("hello there\n".into())]
                    }
                ))),
            )
        }
    });

    // After a while, simulate terminating the connection from the client side.
    tokio::time::sleep(Duration::from_millis(500)).await;
    proxy
        .send(IncomingProxyMessage::AgentSteal(DaemonTcp::Close(
            TcpClose { connection_id: 0 },
        )))
        .await;

    // Expect the local server to be notified.
    tokio::time::timeout(Duration::from_millis(250), local_server_task)
        .await
        .unwrap()
        .unwrap();

    if let Some(Err(error)) = receive_frames.now_or_never() {
        panic!("{error}");
    }
}

/// Verifies session monitor events for a stolen HTTP request with a streamed body:
/// the request head event carries the collision-free exchange id together with port and
/// HTTP version, and a single body event is emitted once the last frame arrives, marked
/// as truncated past the configured capture limit.
#[tokio::test]
async fn emits_request_monitor_events_for_streamed_body() {
    let local_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local_listener.local_addr().unwrap();

    let (conn, _, out) = Connection::dummy();
    let (monitor_raw_tx, mut monitor_rx) = tokio::sync::broadcast::channel(64);
    let proxy = IncomingProxy::new(
        Duration::from_secs(3),
        Default::default(),
        MonitorTx::from_sender_with_body_limit(monitor_raw_tx, 8),
    );
    let mut background_tasks: BackgroundTasks<(), ProxyMessage, IncomingProxyError> =
        BackgroundTasks::new(conn.tx_handle());

    let proxy = background_tasks.register(proxy, (), 8);

    proxy
        .send(IncomingProxyMessage::AgentProtocolVersion(
            mirrord_protocol::VERSION.clone(),
        ))
        .await;
    proxy
        .send(IncomingProxyMessage::LayerRequest(
            0,
            LayerId(0),
            IncomingRequest::PortSubscribe(PortSubscribe {
                listening_on: local_addr.into(),
                subscription: PortSubscription::Steal(StealType::All(80)),
            }),
        ))
        .await;
    assert_eq!(
        out.next().await.unwrap(),
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(80))),
    );
    proxy
        .send(IncomingProxyMessage::AgentSteal(
            DaemonTcp::SubscribeResult(Ok(80)),
        ))
        .await;
    assert_eq!(
        background_tasks.next().await.unwrap().1.unwrap_message(),
        ProxyMessage::ToLayer(ToLayer {
            message_id: 0,
            layer_id: LayerId(0),
            message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(())))
        }),
    );

    let local_server_task = tokio::spawn(async move {
        let (conn, _) = local_listener.accept().await.unwrap();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(conn),
                service_fn(|mut req: Request<Incoming>| async move {
                    while let Some(frame) = req.body_mut().frame().await {
                        frame.unwrap();
                    }
                    Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new()))
                }),
            )
            .await
    });

    proxy
        .send(IncomingProxyMessage::AgentSteal(
            DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                connection_id: 0,
                request_id: 0,
                metadata: HttpRequestMetadata::V1 {
                    source: "127.0.0.1:55555".parse().unwrap(),
                    destination: "127.0.0.1:80".parse().unwrap(),
                },
                transport: IncomingTrafficTransportType::Tcp,
                request: InternalHttpRequest {
                    method: Method::POST,
                    uri: "/api/items".parse().unwrap(),
                    version: Version::HTTP_11,
                    headers: [("host", "myapp.test"), ("content-length", "13")]
                        .into_iter()
                        .map(|(name, value)| (name.parse().unwrap(), value.parse().unwrap()))
                        .collect(),
                    body: InternalHttpBodyNew {
                        frames: vec![InternalHttpBodyFrame::Data("hello ".into())],
                        is_last: false,
                    },
                },
            })),
        ))
        .await;
    proxy
        .send(IncomingProxyMessage::AgentSteal(
            DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                frames: vec![InternalHttpBodyFrame::Data("world!!".into())],
                is_last: true,
                connection_id: 0,
                request_id: 0,
            })),
        ))
        .await;

    match out.next().await.unwrap() {
        ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(response)) => {
            assert_eq!(response.internal_response.status, StatusCode::OK);
        }
        other => panic!("unexpected message: {other:?}"),
    }

    local_server_task.abort();

    match monitor_rx.try_recv().unwrap() {
        MonitorEvent::IncomingRequest {
            id,
            method,
            path,
            host,
            port,
            http_version,
            headers,
        } => {
            assert_eq!(id, "0:s:0:0");
            assert_eq!(method, "POST");
            assert_eq!(path, "/api/items");
            assert_eq!(host, "myapp.test");
            assert_eq!(port, 80);
            assert_eq!(http_version, "HTTP/1.1");
            assert!(
                headers
                    .iter()
                    .any(|(name, value)| name == "host" && value == "myapp.test")
            );
        }
        other => panic!("unexpected monitor event: {other:?}"),
    }

    match monitor_rx.try_recv().unwrap() {
        MonitorEvent::IncomingRequestBody {
            id,
            method,
            path,
            body,
            truncated,
            bytes,
        } => {
            assert_eq!(id, "0:s:0:0");
            assert_eq!(method, "POST");
            assert_eq!(path, "/api/items");
            assert_eq!(body, "hello wo");
            assert!(truncated);
            assert_eq!(bytes, 13);
        }
        other => panic!("unexpected monitor event: {other:?}"),
    }

    match monitor_rx.try_recv().unwrap() {
        MonitorEvent::IncomingResponse { id, status, .. } => {
            assert_eq!(id, "0:s:0:0");
            assert_eq!(status, 200);
        }
        other => panic!("unexpected monitor event: {other:?}"),
    }
    assert!(monitor_rx.try_recv().is_err());
}
