//! Tests verifying our implementation of concurrent stealing and mirroring of the incoming traffic.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use http_body_util::{combinators::BoxBody, BodyExt, StreamBody};
use hyper::{
    body::Frame,
    http::{header, HeaderValue, Method, StatusCode},
    Request,
};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedResponse, DaemonTcp, Filter, HttpFilter,
        HttpRequestMetadata, HttpResponse, InternalHttpBodyFrame, InternalHttpBodyNew,
        InternalHttpResponse, LayerTcpSteal, StealType, TcpClose, TcpData,
    },
    DaemonMessage,
};
use rstest::rstest;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, Notify},
    task::JoinSet,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use super::test_service::{self, full_body};
use crate::{
    http::HttpVersion,
    incoming::test::start_redirector_task,
    mirror::TcpMirrorApi,
    steal::{TcpStealApi, TcpStealerTask},
    test::{get_mirror_api_with_subscription, get_steal_api_with_subscription},
    util::remote_runtime::{BgTaskRuntime, IntoStatus},
};

/// Tests a scenario where there are:
/// 1. Two stealing clients, each with a header HTTP filter.
/// 2. One mirroring client.
///
/// The HTTP client sends a total of 3 or 6 requests, depending on HTTP version:
/// 1. 3 requests that result in no HTTP upgrade.
/// 2. (HTTP/1 only) 3 requests that result in an HTTP upgrade.
///
/// The 1. and 4. request should not be stolen.
/// The other requests should be stolen by one of the stealing clients.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn filtered_stealing_with_mirroring(
    #[values(HttpVersion::V1, HttpVersion::V2)] http_version: HttpVersion,
) {
    let (mut connections, steal_handle, mirror_handle) =
        start_redirector_task(Default::default()).await;
    let (stealer_tx, stealer_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(steal_handle, stealer_rx);
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer task");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap();
    let (request_tx, mut request_rx) = mpsc::channel(8);
    let (steal_1, steal_2, mut mirror) = tokio::join!(
        get_steal_api_with_subscription(
            1,
            stealer_tx.clone(),
            stealer_status.clone(),
            Some("1.19.4"),
            StealType::FilteredHttpEx(
                destination.port(),
                HttpFilter::Header(Filter::new("x-client: 1".into()).unwrap())
            )
        )
        .map(|res| res.0),
        get_steal_api_with_subscription(
            2,
            stealer_tx.clone(),
            stealer_status.clone(),
            Some("1.19.4"),
            StealType::FilteredHttpEx(
                destination.port(),
                HttpFilter::Header(Filter::new("x-client: 2".into()).unwrap())
            )
        )
        .map(|res| res.0),
        get_mirror_api_with_subscription(mirror_handle, Some("1.19.4"), destination.port())
            .map(|res| res.0),
    );

    let mut tasks = JoinSet::new();
    let token = CancellationToken::new();
    let token_clone = token.clone();

    // Original destination HTTP server.
    tasks.spawn(async move {
        tokio::join!(
            test_service::run_test_service(listener, http_version, request_tx, token_clone,),
            async move {
                // Handle first request, with no upgrade.
                let mut request = request_rx.recv().await.unwrap();
                println!(
                    "Original destination: received the first request {:?}",
                    request.request
                );
                request.assert_header("x-request-id", Some("0"));
                request.assert_header("x-client", Some("0"));
                request.assert_body(&[0]).await;
                request.respond_simple(StatusCode::OK, &[0]);
                println!("Original destination: first request finished");

                if http_version == HttpVersion::V2 {
                    // HTTP/2 has no upgrades.
                    return;
                }

                // Handle second request, with upgrade.
                let mut request = request_rx.recv().await.unwrap();
                println!(
                    "Original destination: received the second request {:?}",
                    request.request
                );
                request.assert_header("x-request-id", Some("3"));
                request.assert_header("x-client", Some("0"));
                request.assert_header("connection", Some("upgrade"));
                request.assert_header("upgrade", Some("dummy"));
                request.assert_body(&[]).await;
                let mut upgraded = request.respond_expect_upgrade("dummy").await;
                upgraded.write_all(&[0]).await.unwrap();
                upgraded.shutdown().await.unwrap();
                println!("Original destination: second request finished");
            },
        );
    });

    // HTTP client.
    tasks.spawn(async move {
        let mut request_sender = connections.new_http(destination, http_version).await;

        // Send 3 simple requests (original dst / client 1 / client 2).
        for i in 0_u8..3 {
            let request = Request::builder()
                .method(Method::GET)
                .uri("http://some-server.com")
                .header("x-request-id", i.to_string())
                .header("x-client", (i % 3).to_string())
                .body(full_body([i]))
                .unwrap();
            println!("HTTP client: sending request {i} {request:?}");
            let (parts, body) = request_sender.send(request).await.unwrap().into_parts();
            assert_eq!(parts.status, StatusCode::OK);
            let body = body.collect().await.unwrap().to_bytes();
            assert_eq!(body, &[i][..]);
            println!("HTTP client: request {i} finished");
        }

        if http_version == HttpVersion::V2 {
            // HTTP/2 has no upgrades.
            token.cancel();
            return;
        }

        // Send 3 upgrade requests (original dst / client 1 / client 2).
        for i in 3_u8..6 {
            let mut request_sender = connections.new_http(destination, http_version).await;

            let request = Request::builder()
                .method(Method::GET)
                .uri("http://some-server.com")
                .header("x-request-id", i.to_string())
                .header("x-client", (i - 3).to_string())
                .header("connection", "upgrade")
                .header("upgrade", "dummy")
                .body(full_body([]))
                .unwrap();
            println!("HTTP client: sending request {i}: {request:?}");
            let mut response = request_sender.send(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
            let upgrade = hyper::upgrade::on(&mut response);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert!(body.is_empty());
            let mut upgraded = TokioIo::new(upgrade.await.unwrap());
            let mut buf = vec![];
            upgraded.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, &[i - 3][..]);
            upgraded.shutdown().await.unwrap();
            println!("HTTP client: request {i} finished");
        }

        token.cancel();
    });

    async fn stealing_client_logic(id: u8, mut api: TcpStealApi, http_version: HttpVersion) {
        // Handle first request, with no upgrade.
        let request = match api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                request,
            ))) => request,
            other => panic!("unexpected message: {other:?}"),
        };
        println!("Stealing client {id}: received the first request {request:?}");
        assert_eq!(
            request.request.headers.get("x-request-id").unwrap(),
            id.to_string().as_str()
        );
        assert_eq!(
            request.request.headers.get("x-client").unwrap(),
            id.to_string().as_str()
        );
        assert_eq!(
            request.request.body,
            InternalHttpBodyNew {
                frames: vec![InternalHttpBodyFrame::Data(vec![id])],
                is_last: true
            }
        );
        let HttpRequestMetadata::V1 { destination, .. } = request.metadata;
        api.handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Start(
            HttpResponse {
                connection_id: request.connection_id,
                request_id: request.request_id,
                port: destination.port(),
                internal_response: InternalHttpResponse {
                    status: StatusCode::OK,
                    headers: Default::default(),
                    version: request.request.version,
                    body: vec![InternalHttpBodyFrame::Data(vec![id])],
                },
            },
        )))
        .await
        .unwrap();
        api.handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
            ChunkedRequestBodyV1 {
                connection_id: request.connection_id,
                request_id: request.request_id,
                frames: Default::default(),
                is_last: true,
            },
        )))
        .await
        .unwrap();
        match api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose { connection_id })) => {
                assert_eq!(connection_id, request.connection_id)
            }
            other => panic!("unexpected message: {other:?}"),
        };
        println!("Stealing client {id}: first request finished");

        if http_version == HttpVersion::V2 {
            // HTTP/2 has no upgrades.
            return;
        }

        // Handle second request, with upgrade.
        let request = match api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                request,
            ))) => request,
            other => panic!("unexpected message: {other:?}"),
        };
        println!("Stealing client {id}: received the second request {request:?}");
        assert_eq!(
            request.request.headers.get("x-request-id").unwrap(),
            (id + 3).to_string().as_str()
        );
        assert_eq!(
            request.request.headers.get("x-client").unwrap(),
            id.to_string().as_str()
        );
        assert_eq!(
            request.request.headers.get("connection").unwrap(),
            "upgrade"
        );
        assert_eq!(request.request.headers.get("upgrade").unwrap(), "dummy");
        assert_eq!(
            request.request.body,
            InternalHttpBodyNew {
                frames: vec![],
                is_last: true
            }
        );
        let HttpRequestMetadata::V1 { destination, .. } = request.metadata;
        api.handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Start(
            HttpResponse {
                connection_id: request.connection_id,
                request_id: request.request_id,
                port: destination.port(),
                internal_response: InternalHttpResponse {
                    status: StatusCode::SWITCHING_PROTOCOLS,
                    headers: [
                        (header::CONNECTION, HeaderValue::from_static("upgrade")),
                        (header::UPGRADE, HeaderValue::from_static("dummy")),
                    ]
                    .into_iter()
                    .collect(),
                    version: request.request.version,
                    body: Default::default(),
                },
            },
        )))
        .await
        .unwrap();
        api.handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
            ChunkedRequestBodyV1 {
                connection_id: request.connection_id,
                request_id: request.request_id,
                frames: Default::default(),
                is_last: true,
            },
        )))
        .await
        .unwrap();
        api.handle_client_message(LayerTcpSteal::Data(TcpData {
            connection_id: request.connection_id,
            bytes: vec![id],
        }))
        .await
        .unwrap();
        api.handle_client_message(LayerTcpSteal::Data(TcpData {
            connection_id: request.connection_id,
            bytes: vec![],
        }))
        .await
        .unwrap();
        match api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                connection_id,
                bytes,
            })) => {
                assert_eq!(connection_id, request.connection_id);
                assert!(bytes.is_empty());
            }
            other => panic!("unexpected message: {other:?}"),
        };
        match api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose { connection_id })) => {
                assert_eq!(connection_id, request.connection_id)
            }
            other => panic!("unexpected message: {other:?}"),
        };
        println!("Stealing client {id}: second request finished");
    }

    // First stealing client.
    tasks.spawn(stealing_client_logic(1, steal_1, http_version));

    // Second stealing client.
    tasks.spawn(stealing_client_logic(2, steal_2, http_version));

    // Mirroring client.
    tasks.spawn(async move {
        // Get first 3 requests, with no upgrade.
        for i in 0_u8..3 {
            let request = match mirror.recv().await.unwrap().unwrap() {
                DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                    request,
                ))) => request,
                other => panic!("unexpected message: {other:?}"),
            };
            println!("Mirroring client: received request {i} {request:?}");
            assert_eq!(
                request.request.headers.get("x-request-id").unwrap(),
                i.to_string().as_str()
            );
            assert_eq!(
                request.request.headers.get("x-client").unwrap(),
                i.to_string().as_str()
            );
            assert_eq!(
                request.request.body,
                InternalHttpBodyNew {
                    frames: vec![InternalHttpBodyFrame::Data(vec![i])],
                    is_last: true
                }
            );
            match mirror.recv().await.unwrap().unwrap() {
                DaemonMessage::Tcp(DaemonTcp::Close(TcpClose { connection_id })) => {
                    assert_eq!(connection_id, request.connection_id)
                }
                other => panic!("unexpected message: {other:?}"),
            };
            println!("Mirroring client: request {i} finished");
        }

        if http_version == HttpVersion::V2 {
            // HTTP/2 has no upgrades.
            return;
        }

        // Get second 3 requests, with upgrades.
        for i in 3_u8..6 {
            let request = match mirror.recv().await.unwrap().unwrap() {
                DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                    request,
                ))) => request,
                other => panic!("unexpected message: {other:?}"),
            };
            println!("Mirroring client: received request {i} {request:?}");
            assert_eq!(
                request.request.headers.get("x-request-id").unwrap(),
                i.to_string().as_str()
            );
            assert_eq!(
                request.request.headers.get("x-client").unwrap(),
                (i - 3).to_string().as_str()
            );
            assert_eq!(
                request.request.headers.get("connection").unwrap(),
                "upgrade"
            );
            assert_eq!(request.request.headers.get("upgrade").unwrap(), "dummy");
            assert_eq!(
                request.request.body,
                InternalHttpBodyNew {
                    frames: vec![],
                    is_last: true
                }
            );
            match mirror.recv().await.unwrap().unwrap() {
                DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                    connection_id,
                    bytes,
                })) => {
                    assert_eq!(connection_id, request.connection_id);
                    assert!(bytes.is_empty());
                }
                other => panic!("unexpected message: {other:?}"),
            };
            match mirror.recv().await.unwrap().unwrap() {
                DaemonMessage::Tcp(DaemonTcp::Close(TcpClose { connection_id })) => {
                    assert_eq!(connection_id, request.connection_id)
                }
                other => panic!("unexpected message: {other:?}"),
            };
            println!("Mirroring client: request {i} finished");
        }
    });

    tasks.join_all().await;
}

/// Verifies that request and response bodies are correctly streamed.
///
/// The HTTP client sends a total of 2 requests:
/// 1. The first request should be passed through to the original destination.
/// 2. The second request should be stolne by a client using an HTTP filter.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_bodies(#[values(HttpVersion::V1, HttpVersion::V2)] http_version: HttpVersion) {
    let (mut connections, steal_handle, mirror_handle) =
        start_redirector_task(Default::default()).await;
    let (stealer_tx, stealer_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(steal_handle, stealer_rx);
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer task");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap();
    let (request_tx, mut request_rx) = mpsc::channel(8);
    let (mut steal, mut mirror) = tokio::join!(
        get_steal_api_with_subscription(
            1,
            stealer_tx.clone(),
            stealer_status.clone(),
            Some("1.19.4"),
            StealType::FilteredHttpEx(
                destination.port(),
                HttpFilter::Header(Filter::new("x-client: 1".into()).unwrap())
            )
        )
        .map(|res| res.0),
        get_mirror_api_with_subscription(mirror_handle, Some("1.19.4"), destination.port())
            .map(|res| res.0),
    );

    let mut tasks = JoinSet::new();
    let token = CancellationToken::new();
    let token_clone = token.clone();

    let notify = Arc::new(Notify::new());

    // Original destination HTTP server.
    let notify_cloned = notify.clone();
    tasks.spawn(async move {
        tokio::join!(
            test_service::run_test_service(listener, http_version, request_tx, token_clone),
            async move {
                let mut request = request_rx.recv().await.unwrap();
                println!(
                    "Original destination: received request {:?}",
                    request.request
                );
                request.assert_header("x-client", Some("0"));
                notify_cloned.notify_one(); // Notify the client to send the first frame.
                request.assert_ready_body(b"hello ").await;
                notify_cloned.notify_one(); // Notify the client to send the second frame.
                request.assert_ready_body(b"there").await;
                notify_cloned.notify_one(); // Notify the client to end the body.
                request.assert_body(&[]).await;

                let body_tx = request.respond_streaming(StatusCode::OK);
                notify_cloned.notified().await; // Wait until the client wants the first frame.
                body_tx.send(b"general ".into()).await.unwrap();
                notify_cloned.notified().await; // Wait until the client wants the second frame.
                body_tx.send(b"kenobi".into()).await.unwrap();
                notify_cloned.notified().await; // Wait until the client wants us to end the body.
                std::mem::drop(body_tx);
            },
        );
    });

    // HTTP client.
    let notify_cloned = notify.clone();
    tasks.spawn(async move {
        for i in 0..2 {
            let mut request_sender = connections.new_http(destination, http_version).await;

            let (body_tx, body_rx) = mpsc::channel::<Vec<u8>>(8);
            let body = BoxBody::new(StreamBody::new(
                ReceiverStream::new(body_rx).map(|data| Ok(Frame::data(Bytes::from(data)))),
            ));
            let mut builder = Request::builder()
                .method(Method::GET)
                .uri("http://some-server.com")
                .header("x-client", i.to_string());
            if http_version == HttpVersion::V1 {
                builder = builder.header(header::TRANSFER_ENCODING, "chunked");
            }
            let request = builder.body(body).unwrap();
            println!("HTTP client: sending request {i} {request:?}");
            let response = tokio::spawn(async move { request_sender.send(request).await.unwrap() });

            notify_cloned.notified().await; // Wait until the server wants the first frame.
            body_tx.send(b"hello ".into()).await.unwrap();
            notify_cloned.notified().await; // Wait until the server wants the second frame.
            body_tx.send(b"there".into()).await.unwrap();
            notify_cloned.notified().await; // Wait until the server wants us to end the body.
            std::mem::drop(body_tx);

            let mut response = response.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            notify_cloned.notify_one(); // Notify the server to send the first frame.
            let data = response
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            assert_eq!(data, b"general ".as_slice());
            notify_cloned.notify_one(); // Notify the server to send the second frame.
            let data = response
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            assert_eq!(data, b"kenobi".as_slice());
            notify_cloned.notify_one(); // Notify the server to end the body.
            assert!(response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty());
            println!("HTTP client: request {i} finished");
        }

        token.cancel();
    });

    // Stealing client.
    let notify_cloned = notify.clone();
    tasks.spawn(async move {
        let request = match steal.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                request,
            ))) => request,
            other => panic!("unexpected message: {other:?}"),
        };
        assert_eq!(request.request.headers.get("x-client").unwrap(), "1",);
        assert_eq!(
            request.request.body,
            InternalHttpBodyNew {
                frames: vec![],
                is_last: false,
            },
        );
        let HttpRequestMetadata::V1 { destination, .. } = request.metadata;

        notify_cloned.notify_one(); // Notify the client to send the first frame.
        assert_eq!(
            steal.recv().await.unwrap(),
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                ChunkedRequestBodyV1 {
                    frames: vec![InternalHttpBodyFrame::Data(b"hello ".into())],
                    is_last: false,
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                }
            ))),
        );
        notify_cloned.notify_one(); // Notify the client to send the second frame.
        assert_eq!(
            steal.recv().await.unwrap(),
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                ChunkedRequestBodyV1 {
                    frames: vec![InternalHttpBodyFrame::Data(b"there".into())],
                    is_last: false,
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                }
            ))),
        );
        notify_cloned.notify_one(); // Notify the client to end the body.
        if http_version == HttpVersion::V2 {
            assert_eq!(
                steal.recv().await.unwrap(),
                DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                    ChunkedRequestBodyV1 {
                        frames: vec![InternalHttpBodyFrame::Data(Default::default())],
                        is_last: false,
                        connection_id: request.connection_id,
                        request_id: request.request_id,
                    }
                ))),
            );
        }
        assert_eq!(
            steal.recv().await.unwrap(),
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                ChunkedRequestBodyV1 {
                    frames: vec![],
                    is_last: true,
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                }
            ))),
        );

        steal
            .handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Start(
                HttpResponse {
                    port: destination.port(),
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    internal_response: InternalHttpResponse {
                        status: StatusCode::OK,
                        version: request.request.version,
                        headers: Default::default(),
                        body: vec![],
                    },
                },
            )))
            .await
            .unwrap();
        notify_cloned.notified().await; // Wait until the client wants us to send the first frame.
        steal
            .handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                ChunkedRequestBodyV1 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    frames: vec![InternalHttpBodyFrame::Data(b"general ".into())],
                    is_last: false,
                },
            )))
            .await
            .unwrap();
        notify_cloned.notified().await; // Wait until the client wants us to send the second frame.
        steal
            .handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                ChunkedRequestBodyV1 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    frames: vec![InternalHttpBodyFrame::Data(b"kenobi".into())],
                    is_last: false,
                },
            )))
            .await
            .unwrap();
        notify_cloned.notified().await; // Wait until the client wants us to end the body.
        steal
            .handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                ChunkedRequestBodyV1 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    frames: vec![],
                    is_last: true,
                },
            )))
            .await
            .unwrap();
        assert_eq!(
            steal.recv().await.unwrap(),
            DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
                connection_id: request.connection_id,
            })),
        );
    });

    // Mirroring client.
    tasks.spawn(async move {
        for i in 0..2 {
            let request = match mirror.recv().await.unwrap().unwrap() {
                DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                    request,
                ))) => request,
                other => panic!("unexpected message: {other:?}"),
            };
            println!("Mirroring client: received request {i} {request:?}");
            assert_eq!(
                request.request.headers.get("x-client").unwrap(),
                i.to_string().as_str()
            );
            assert_eq!(
                request.request.body,
                InternalHttpBodyNew {
                    frames: vec![],
                    is_last: false,
                },
            );
            assert_eq!(
                mirror.recv().await.unwrap().unwrap(),
                DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                    ChunkedRequestBodyV1 {
                        frames: vec![InternalHttpBodyFrame::Data(b"hello ".into())],
                        is_last: false,
                        connection_id: request.connection_id,
                        request_id: request.request_id,
                    }
                ))),
            );
            assert_eq!(
                mirror.recv().await.unwrap().unwrap(),
                DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                    ChunkedRequestBodyV1 {
                        frames: vec![InternalHttpBodyFrame::Data(b"there".into())],
                        is_last: false,
                        connection_id: request.connection_id,
                        request_id: request.request_id,
                    }
                ))),
            );
            if http_version == HttpVersion::V2 {
                assert_eq!(
                    mirror.recv().await.unwrap().unwrap(),
                    DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                        ChunkedRequestBodyV1 {
                            frames: vec![InternalHttpBodyFrame::Data(Default::default())],
                            is_last: false,
                            connection_id: request.connection_id,
                            request_id: request.request_id,
                        }
                    ))),
                );
            }
            assert_eq!(
                mirror.recv().await.unwrap().unwrap(),
                DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                    ChunkedRequestBodyV1 {
                        frames: vec![],
                        is_last: true,
                        connection_id: request.connection_id,
                        request_id: request.request_id,
                    }
                ))),
            );
            assert_eq!(
                mirror.recv().await.unwrap().unwrap(),
                DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                    connection_id: request.connection_id,
                })),
            );
        }
    });

    tasks.join_all().await;
}
