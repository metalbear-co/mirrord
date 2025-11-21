use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::{StreamBody, combinators::BoxBody};
use hyper::{
    Method, Request, Response, Version,
    body::{Frame, Incoming},
    service::Service,
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
use rstest::rstest;
use tokio::net::TcpListener;

use crate::{
    background_tasks::BackgroundTasks,
    main_tasks::{ProxyMessage, ToLayer},
    proxies::incoming::{IncomingProxy, IncomingProxyError, IncomingProxyMessage},
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

    let proxy = IncomingProxy::new(Duration::from_secs(3), Default::default());
    let mut background_tasks: BackgroundTasks<(), ProxyMessage, IncomingProxyError> =
        Default::default();
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
                listening_on: local_addr,
                subscription: PortSubscription::Steal(steal_type.clone()),
            }),
        ))
        .await;
    assert_eq!(
        background_tasks.next().await.unwrap().1.unwrap_message(),
        ProxyMessage::ToAgent(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
            steal_type
        ))),
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
            res = conn => if let Ok(..) = res {
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
        let message = background_tasks.next().await.unwrap().1.unwrap_message();
        match message {
            ProxyMessage::ToAgent(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                ChunkedResponse::Start(response),
            ))) => {
                assert_eq!(response.connection_id, 0);
                assert_eq!(response.request_id, 0);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        loop {
            assert_eq!(
                background_tasks.next().await.unwrap().1.unwrap_message(),
                ProxyMessage::ToAgent(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                    ChunkedResponse::Body(ChunkedRequestBodyV1 {
                        is_last: false,
                        connection_id: 0,
                        request_id: 0,
                        frames: vec![InternalHttpBodyFrame::Data("hello there\n".into())]
                    })
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
