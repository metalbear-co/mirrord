//! Basic [`RedirectorTask`](crate::incoming::RedirectorTask) tests.

use std::{
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    sync::Arc,
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http_body_util::{BodyExt, Empty};
use hyper::{
    body::Incoming,
    http::{header, HeaderValue, Method, StatusCode, Uri, Version},
    Request, Response,
};
use hyper_util::rt::TokioIo;
use rstest::rstest;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, Interest},
    net::TcpListener,
    sync::Notify,
};
use tokio_util::sync::CancellationToken;

use crate::{
    http::HttpVersion,
    incoming::{
        connection::{
            http::{RedirectedHttp, RequestHead},
            tcp::RedirectedTcp,
        },
        test::start_redirector_task,
        BoxBody, IncomingStream, IncomingStreamItem, MirroredHttp, MirroredTcp, MirroredTraffic,
        StolenTraffic,
    },
};

impl MirroredTraffic {
    fn unwrap_tcp(self) -> MirroredTcp {
        match self {
            MirroredTraffic::Tcp(tcp) => tcp,
            MirroredTraffic::Http(..) => panic!("expected TCP, got HTTP"),
        }
    }

    fn unwrap_http(self) -> MirroredHttp {
        match self {
            MirroredTraffic::Http(http) => http,
            MirroredTraffic::Tcp(..) => panic!("expected HTTP, got TCP"),
        }
    }
}

impl StolenTraffic {
    fn unwrap_tcp(self) -> RedirectedTcp {
        match self {
            Self::Tcp(tcp) => tcp,
            Self::Http(..) => panic!("expected TCP, got HTTP"),
        }
    }

    fn unwrap_http(self) -> RedirectedHttp {
        match self {
            Self::Http(http) => http,
            Self::Tcp(..) => panic!("expected HTTP, got TCP"),
        }
    }
}

/// Verifies that the [`RedirectorTask`](crate::incoming::RedirectorTask) cleans up its state when
/// handle's traffic channels (port subscriptions) are dropped.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn cleanup_on_dead_channel() {
    let (mut connections, mut steal_handle, mut mirror_handle) =
        start_redirector_task(Default::default()).await;

    steal_handle.steal(80).await.unwrap();
    assert!(connections
        .redirector_state()
        .borrow()
        .has_redirections([80]));

    steal_handle.stop_steal(80);
    connections
        .redirector_state()
        .wait_for(|state| state.has_redirections([]))
        .await
        .unwrap();

    steal_handle.steal(81).await.unwrap();
    assert!(connections
        .redirector_state()
        .borrow()
        .has_redirections([81]));

    mirror_handle.mirror(81).await.unwrap();
    mirror_handle.mirror(80).await.unwrap();
    assert!(connections
        .redirector_state()
        .borrow()
        .has_redirections([80, 81]));

    std::mem::drop(steal_handle);

    mirror_handle.mirror(82).await.unwrap();
    assert!(connections
        .redirector_state()
        .borrow()
        .has_redirections([80, 81, 82]));

    std::mem::drop(mirror_handle);

    connections
        .redirector_state()
        .wait_for(|state| state.has_redirections([]))
        .await
        .unwrap();
}

/// Verifies that the [`RedirectorTask`](crate::incoming::RedirectorTask) correctly distributes
/// incoming TCP traffic between the subscribed clients.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn concurrent_mirror_and_steal_tcp() {
    let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

    let (mut connections, mut steal_handle, mut mirror_handle) =
        start_redirector_task(Default::default()).await;

    // Both handles should receive traffic.
    steal_handle.steal(destination.port()).await.unwrap();
    mirror_handle.mirror(destination.port()).await.unwrap();
    let mut conn = connections.new_tcp(destination).await;
    let peer_addr = conn.local_addr().unwrap();
    conn.write_all(b"hello, this is not tcp").await.unwrap();
    let redirected = steal_handle.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(redirected.info().original_destination, destination);
    assert_eq!(redirected.info().peer_addr, peer_addr);
    assert!(redirected.info().tls_connector.is_none());
    let mirrored = mirror_handle.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(mirrored.info.original_destination, destination);
    assert!(mirrored.info.tls_connector.is_none());

    // New mirror handle should not inherit parent's subscriptions,
    // and should not receive traffic.
    let mut mirror_handle_2 = mirror_handle.clone();
    let mut conn = connections.new_tcp(destination).await;
    let peer_addr = conn.local_addr().unwrap();
    conn.write_all(b"hello, this is not tcp").await.unwrap();
    let redirected = steal_handle.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(redirected.info().original_destination, destination);
    assert_eq!(redirected.info().peer_addr, peer_addr);
    assert!(redirected.info().tls_connector.is_none());
    let mirrored = mirror_handle.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(mirrored.info.original_destination, destination);
    assert!(mirrored.info.tls_connector.is_none());
    assert!(mirror_handle_2.next().await.is_none());

    // New mirror handle should receive traffic after making the subscription.
    mirror_handle_2.mirror(destination.port()).await.unwrap();
    let mut conn = connections.new_tcp(destination).await;
    let peer_addr = conn.local_addr().unwrap();
    conn.write_all(b"hello, this is not tcp").await.unwrap();
    let redirected = steal_handle.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(redirected.info().original_destination, destination);
    assert_eq!(redirected.info().peer_addr, peer_addr);
    assert!(redirected.info().tls_connector.is_none());
    let mirrored = mirror_handle.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(mirrored.info.original_destination, destination);
    assert!(mirrored.info.tls_connector.is_none());
    let mirrored = mirror_handle_2.next().await.unwrap().unwrap().unwrap_tcp();
    assert_eq!(mirrored.info.original_destination, destination);
    assert!(mirrored.info.tls_connector.is_none());

    // Redirection should be removed when all subscriptions are dropped.
    steal_handle.stop_steal(destination.port());
    mirror_handle.stop_mirror(destination.port());
    mirror_handle_2.stop_mirror(destination.port());
    connections
        .redirector_state()
        .wait_for(|state| state.has_redirections([]))
        .await
        .unwrap();
    // Verify that the redirector task drops the connection
    // returned from our port redirector impl.
    let conn = connections.new_tcp(destination).await;
    let ready = conn.ready(Interest::READABLE).await.unwrap();
    assert!(ready.is_read_closed());
}

/// Verifies that the [`RedirectorTask`](crate::incoming::RedirectorTask) correctly distributes
/// incoming HTTP requests between the subscribed clients,
/// and handles subscriptions starting in the middle of a redirected HTTP connection.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn concurrent_mirror_and_steal_http() {
    async fn assert_empty_request(head: RequestHead, incoming_stream: IncomingStream) {
        assert_eq!(head.uri, Uri::from_static("/"));
        assert_eq!(head.method, Method::GET);
        assert_eq!(head.version, Version::HTTP_11);
        assert!(head.body.is_empty());
        assert!(head.has_more_frames.not());

        let items = incoming_stream.collect::<Vec<_>>().await;
        assert!(matches!(
            items.as_slice(),
            [IncomingStreamItem::Finished(Ok(()))],
        ))
    }

    let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

    let (mut connections, mut steal_handle, mut mirror_handle) =
        start_redirector_task(Default::default()).await;

    steal_handle.steal(80).await.unwrap();

    let mut sender = connections.new_http(destination, HttpVersion::V1).await;
    let notify = Arc::new(Notify::new());
    let notify_clone = notify.clone();
    let sender = tokio::spawn(async move {
        for _ in 0..3 {
            notify_clone.notified().await;
            let request = Request::builder()
                .version(Version::HTTP_11)
                .method(Method::GET)
                .uri("/")
                .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
                .unwrap();
            let mut response = sender.send(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.body_mut().frame().await.is_none());
        }
    });

    let response = Response::builder()
        .version(Version::HTTP_11)
        .status(StatusCode::OK)
        .body(Empty::<Bytes>::new().map_err(|_| unreachable!()))
        .unwrap();

    notify.notify_one();
    let stolen = steal_handle
        .next()
        .await
        .unwrap()
        .unwrap()
        .unwrap_http()
        .steal();
    stolen
        .response_provider
        .send(response.clone().map(BoxBody::new));
    assert_empty_request(stolen.request_head, stolen.stream).await;

    mirror_handle.mirror(80).await.unwrap();
    notify.notify_one();
    let stolen = steal_handle
        .next()
        .await
        .unwrap()
        .unwrap()
        .unwrap_http()
        .steal();
    stolen
        .response_provider
        .send(response.clone().map(BoxBody::new));
    assert_empty_request(stolen.request_head, stolen.stream).await;
    let mirrored = mirror_handle.next().await.unwrap().unwrap().unwrap_http();
    assert_empty_request(mirrored.request_head, mirrored.stream).await;

    let mut mirror_handle_2 = mirror_handle.clone();
    mirror_handle_2.mirror(80).await.unwrap();
    notify.notify_one();
    let stolen = steal_handle
        .next()
        .await
        .unwrap()
        .unwrap()
        .unwrap_http()
        .steal();
    stolen
        .response_provider
        .send(response.clone().map(BoxBody::new));
    assert_empty_request(stolen.request_head, stolen.stream).await;
    let mirrored_1 = mirror_handle.next().await.unwrap().unwrap().unwrap_http();
    assert_empty_request(mirrored_1.request_head, mirrored_1.stream).await;
    let mirrored_2 = mirror_handle_2.next().await.unwrap().unwrap().unwrap_http();
    assert_empty_request(mirrored_2.request_head, mirrored_2.stream).await;

    sender.await.unwrap();
}

/// Verifies that the [`RedirectorTask`](crate::incoming::RedirectorTask) correctly distributes
/// incoming HTTP requests with upgrades between the subscribed clients.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn concurrent_mirror_and_steal_http_upgrade() {
    let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

    let (mut connections, mut steal_handle, mut mirror_handle) =
        start_redirector_task(Default::default()).await;

    steal_handle.steal(80).await.unwrap();
    mirror_handle.mirror(80).await.unwrap();

    let mut sender = connections.new_http(destination, HttpVersion::V1).await;
    let sender = tokio::spawn(async move {
        let request = Request::builder()
            .version(Version::HTTP_11)
            .method(Method::GET)
            .uri("/")
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "echo")
            .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
            .unwrap();
        let mut response = sender.send(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response
                .headers()
                .get(header::CONNECTION)
                .map(HeaderValue::as_bytes),
            Some(b"upgrade".as_slice())
        );
        assert_eq!(
            response
                .headers()
                .get(header::UPGRADE)
                .map(HeaderValue::as_bytes),
            Some(b"echo".as_slice())
        );
        assert!(response.body_mut().frame().await.is_none());

        let upgraded = hyper::upgrade::on(&mut response).await.unwrap();
        let mut upgraded = TokioIo::new(upgraded);

        let mut buf = BytesMut::with_capacity(128);
        loop {
            upgraded.read_buf(&mut buf).await.unwrap();
            if buf.is_empty() {
                upgraded.shutdown().await.unwrap();
                break;
            }

            upgraded.write_all(buf.as_ref()).await.unwrap();
            buf.clear();
        }
    });

    let mut stolen = steal_handle
        .next()
        .await
        .unwrap()
        .unwrap()
        .unwrap_http()
        .steal();
    let mut mirrored = mirror_handle.next().await.unwrap().unwrap().unwrap_http();

    let data_tx = stolen.response_provider.send_with_upgrade(
        Response::builder()
            .version(Version::HTTP_11)
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "echo")
            .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
            .unwrap(),
    );

    for message in [b"hello 1", b"hello 2"] {
        data_tx.send(message.into()).await.unwrap();
        for stream in [&mut stolen.stream, &mut mirrored.stream] {
            let item = stream.next().await.unwrap();
            assert!(matches!(
                item,
                IncomingStreamItem::Data(data) if data == message,
            ));
        }
    }

    std::mem::drop(data_tx);
    for stream in [&mut stolen.stream, &mut mirrored.stream] {
        let item = stream.next().await.unwrap();
        assert!(matches!(item, IncomingStreamItem::NoMoreData,));
        let item = stream.next().await.unwrap();
        assert!(matches!(item, IncomingStreamItem::Finished(Ok(())),));
    }

    sender.await.unwrap();
}

/// Verifies that the [`RedirectorTask`](crate::incoming::RedirectorTask) correctly
/// passes an incoming TCP connection to the original destination,
/// given that there is no stealing client.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn passthrough_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap();

    let (mut connections, _, mut mirror_handle) = start_redirector_task(Default::default()).await;

    mirror_handle.mirror(destination.port()).await.unwrap();

    let mut client_stream = connections.new_tcp(destination).await;
    client_stream
        .write_all(b"hello, this is tcp")
        .await
        .unwrap();
    client_stream.shutdown().await.unwrap();
    let mut server_stream = listener.accept().await.unwrap().0;
    let mut mirrored_stream = mirror_handle.next().await.unwrap().unwrap().unwrap_tcp();

    let mut buf = Vec::new();
    server_stream.read_to_end(&mut buf).await.unwrap();
    assert_eq!(buf, b"hello, this is tcp");

    buf.clear();
    loop {
        let data_received = buf == b"hello, this is tcp";
        match mirrored_stream.stream.next().await.unwrap() {
            IncomingStreamItem::Data(data) if data_received.not() => {
                buf.extend_from_slice(&data);
            }
            IncomingStreamItem::NoMoreData if data_received => {
                break;
            }
            other => panic!("unexpected stream item: {other:?}"),
        }
    }

    server_stream.write_all(b"hello back").await.unwrap();
    server_stream.shutdown().await.unwrap();

    let item = mirrored_stream.stream.next().await.unwrap();
    assert!(matches!(item, IncomingStreamItem::Finished(Ok(()))));

    buf.clear();
    client_stream.read_to_end(&mut buf).await.unwrap();
    assert_eq!(buf, b"hello back");
}

/// Verifies that the [`RedirectorTask`](crate::incoming::RedirectorTask) correctly
/// passes an incoming HTTP request to the original destination,
/// given that there is no stealing client.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn passthrough_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap();

    let token = CancellationToken::new();
    let token_cloned = token.clone();
    let server = tokio::spawn(async move {
        let result = tokio::select! {
            _ = token_cloned.cancelled() => return,
            result = listener.accept() => result,
        };

        let stream = result.unwrap().0;
        let shutdown = token_cloned.clone().cancelled_owned();
        let service = hyper::service::service_fn(|_request: Request<Incoming>| {
            std::future::ready(Ok(Response::builder()
                .status(StatusCode::OK)
                .version(Version::HTTP_11)
                .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
                .unwrap()))
        });
        crate::http::run_http_server(stream, service, HttpVersion::V1, shutdown)
            .await
            .unwrap();
    });

    let (mut connections, _, mut mirror_handle) = start_redirector_task(Default::default()).await;

    mirror_handle.mirror(destination.port()).await.unwrap();

    let mut sender = connections.new_http(destination, HttpVersion::V1).await;
    let response = sender
        .send(
            Request::builder()
                .method(Method::GET)
                .uri("/some/path")
                .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(response.status(), StatusCode::OK);
    let mut mirrored = mirror_handle.next().await.unwrap().unwrap().unwrap_http();
    assert_eq!(mirrored.request_head.uri, Uri::from_static("/some/path"));
    assert!(mirrored.request_head.body.is_empty());
    assert!(mirrored.request_head.has_more_frames.not());
    let item = mirrored.stream.next().await.unwrap();
    assert!(matches!(item, IncomingStreamItem::Finished(Ok(()))));

    token.cancel();
    server.await.unwrap();
}
