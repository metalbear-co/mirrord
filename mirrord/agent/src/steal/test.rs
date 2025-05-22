//! Tests for the whole [`steal`](super) module.
//!
//! Verify the whole flow from the [`TcpStealerApi`](super::api::TcpStealerApi)
//! to the [`RedirectorTask`](crate::incoming::RedirectorTask).

use std::time::Duration;

use futures::StreamExt;
use mirrord_protocol::{
    tcp::{DaemonTcp, Filter, HttpFilter, IncomingTrafficTransportType, StealType},
    DaemonMessage, LogLevel,
};
use mirrord_tls_util::MaybeTls;
use rstest::rstest;
use rustls::pki_types::ServerName;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_rustls::TlsStream;
use utils::{StealingClient, TestHttpKind, TestRequest, TestTcpProtocol};

use super::TcpStealerTask;
use crate::{
    incoming::{test::DummyRedirector, tls::test::SimpleStore, RedirectorTask},
    util::remote_runtime::{BgTaskRuntime, IntoStatus},
};

mod utils;

/// Verifies that redirected request upgrades are handled correctly.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn request_upgrade(
    #[values(false, true)] stolen: bool,
    #[values(
        TestHttpKind::Http1,
        TestHttpKind::Http1Alpn,
        TestHttpKind::Http1NoAlpn
    )]
    http_kind: TestHttpKind,
    #[values(
        TestTcpProtocol::Echo,
        TestTcpProtocol::ClientTalks,
        TestTcpProtocol::ServerTalks
    )]
    upgraded_protocol: TestTcpProtocol,
) {
    // Initial setup
    let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_destination = original_server.local_addr().unwrap();
    let tls_setup = if http_kind.uses_tls() {
        Some(SimpleStore::new(original_destination.port(), &["h2", "http/1.1"]).await)
    } else {
        None
    };
    let (redirector, _redirector_state, mut conn_tx) = DummyRedirector::new();
    let (redirector, handle) = RedirectorTask::new(
        redirector,
        tls_setup
            .as_ref()
            .map(|setup| setup.store.clone())
            .unwrap_or_default(),
    );
    let (command_tx, command_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(command_rx, handle);
    tokio::spawn(redirector.run());
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer");

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: if stolen { 0 } else { 1 },
        upgrade: Some(upgraded_protocol),
        kind: http_kind,
        connector: tls_setup.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: tls_setup.as_ref().map(SimpleStore::acceptor),
    };

    let mut stealing_client = StealingClient::new(
        0,
        command_tx,
        "1.19.4",
        StealType::FilteredHttpEx(
            original_destination.port(),
            HttpFilter::Header(Filter::new(format!("{}: 0", TestRequest::USER_ID_HEADER)).unwrap()),
        ),
        stealer_status,
    )
    .await;
    let conn = conn_tx.make_connection(original_destination).await;

    tokio::join!(
        async {
            let mut sender = request.make_connection(conn).await;
            request.send(&mut sender, if stolen { 0 } else { 1 }).await;
        },
        async {
            if stolen {
                stealing_client.expect_request(&request).await;
            } else {
                let (stream, _) = original_server.accept().await.unwrap();
                request.accept(stream, if stolen { 0 } else { 1 }).await;
            }
        },
    );
}

/// Verifies scenario where the request cannot be stolen, because the client has an unfiltered
/// subscription, and their mirrord-protocol version is too low.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn http_with_unfiltered_subscription(
    #[values(
        TestHttpKind::Http1,
        TestHttpKind::Http1Alpn,
        TestHttpKind::Http1NoAlpn,
        TestHttpKind::Http2,
        TestHttpKind::Http2Alpn,
        TestHttpKind::Http2NoAlpn
    )]
    http_kind: TestHttpKind,
) {
    // Initial setup
    let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_destination = original_server.local_addr().unwrap();
    let tls_setup = if http_kind.uses_tls() {
        Some(SimpleStore::new(original_destination.port(), &["h2", "http/1.1"]).await)
    } else {
        None
    };
    let (redirector, _redirector_state, mut conn_tx) = DummyRedirector::new();
    let (redirector, handle) = RedirectorTask::new(
        redirector,
        tls_setup
            .as_ref()
            .map(|setup| setup.store.clone())
            .unwrap_or_default(),
    );
    let (command_tx, command_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(command_rx, handle);
    tokio::spawn(redirector.run());
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer");

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: tls_setup.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: tls_setup.as_ref().map(SimpleStore::acceptor),
    };

    let mut client_1 = StealingClient::new(
        0,
        command_tx.clone(),
        "1.19.3", // not high enough for stealing requests with unfiltered subscription
        StealType::All(original_destination.port()),
        stealer_status.clone(),
    )
    .await;
    let conn = conn_tx.make_connection(original_destination).await;
    let mut sender = request.make_connection(conn).await;
    tokio::join!(
        request.send(&mut sender, 2137),
        async {
            let (stream, _) = original_server.accept().await.unwrap();
            request.accept(stream, 2137).await;
        },
        async {
            client_1
                .expect_log(LogLevel::Error, "mirrord-protocol version requirement")
                .await;
        },
    );

    let mut client_2 = StealingClient::new(
        1,
        command_tx.clone(),
        "1.19.4", // high enough for stealing requests with unfiltered subscription
        StealType::All(original_destination.port()),
        stealer_status.clone(),
    )
    .await;
    tokio::join!(
        request.send(&mut sender, 1),
        client_2.expect_request(&request),
    );
}

/// Verifies stealing and passthrough of TCP connections.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn tcp_stealing(#[values(false, true)] with_tls: bool, #[values(false, true)] stolen: bool) {
    // Initial setup
    let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_destination = original_server.local_addr().unwrap();
    let tls_setup = if with_tls {
        Some(SimpleStore::new(original_destination.port(), &[]).await)
    } else {
        None
    };
    let (redirector, _redirector_state, mut conn_tx) = DummyRedirector::new();
    let (redirector, handle) = RedirectorTask::new(
        redirector,
        tls_setup
            .as_ref()
            .map(|setup| setup.store.clone())
            .unwrap_or_default(),
    );
    let (command_tx, command_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(command_rx, handle);
    tokio::spawn(redirector.run());
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer");

    let steal_type = if stolen {
        StealType::All(original_destination.port())
    } else {
        StealType::FilteredHttpEx(
            original_destination.port(),
            HttpFilter::Header(Filter::new("whatever".into()).unwrap()),
        )
    };
    let mut client = StealingClient::new(
        0,
        command_tx.clone(),
        "1.19.4",
        steal_type,
        stealer_status.clone(),
    )
    .await;

    let conn = conn_tx.make_connection(original_destination).await;
    tokio::join!(
        async {
            let conn = if let Some(tls_setup) = &tls_setup {
                let connector = tls_setup.connector(None);
                let conn = connector
                    .connect(ServerName::try_from("server").unwrap(), conn)
                    .await
                    .unwrap();
                MaybeTls::Tls(Box::new(TlsStream::Client(conn)))
            } else {
                MaybeTls::NoTls(conn)
            };
            TestTcpProtocol::Echo.run(conn, false).await;
        },
        async {
            if stolen {
                let conn = client.expect_connection().await;
                if with_tls {
                    assert_eq!(
                        conn.transport,
                        IncomingTrafficTransportType::Tls {
                            alpn_protocol: None,
                            server_name: Some("server".into())
                        }
                    );
                } else {
                    assert_eq!(conn.transport, IncomingTrafficTransportType::Tcp);
                }

                client
                    .expect_tcp(conn.connection.connection_id, TestTcpProtocol::Echo)
                    .await;
            } else {
                let (conn, _) = original_server.accept().await.unwrap();
                let conn = if let Some(tls_setup) = &tls_setup {
                    let acceptor = tls_setup.acceptor();
                    let conn = acceptor.accept(conn).await.unwrap();
                    MaybeTls::Tls(Box::new(TlsStream::Server(conn)))
                } else {
                    MaybeTls::NoTls(conn)
                };
                TestTcpProtocol::Echo.run(conn, true).await;
            }
        },
    );
}

/// Verifies scenario where the client cannot steal a TLS connection,
/// because their mirrord-protocol version is too low.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn tls_protocol_version_check() {
    // Initial setup
    let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_destination = original_server.local_addr().unwrap();
    let tls_setup = SimpleStore::new(original_destination.port(), &[]).await;
    let (redirector, _redirector_state, mut conn_tx) = DummyRedirector::new();
    let (redirector, handle) = RedirectorTask::new(redirector, tls_setup.store.clone());
    let (command_tx, command_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(command_rx, handle);
    tokio::spawn(redirector.run());
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer");

    let mut client = StealingClient::new(
        0,
        command_tx.clone(),
        "1.19.3", // not high enough for stealing TLS connections
        StealType::All(original_destination.port()),
        stealer_status.clone(),
    )
    .await;

    let conn = conn_tx.make_connection(original_destination).await;
    tokio::join!(
        async {
            let connector = tls_setup.connector(None);
            let conn = connector
                .connect(ServerName::try_from("server").unwrap(), conn)
                .await
                .unwrap();
            TestTcpProtocol::Echo.run(conn, false).await;
        },
        async {
            let (conn, _) = original_server.accept().await.unwrap();
            let acceptor = tls_setup.acceptor();
            let conn = acceptor.accept(conn).await.unwrap();
            TestTcpProtocol::Echo.run(conn, true).await;
        },
        client.expect_log(LogLevel::Error, "mirrord-protocol version requirement"),
    );
}

/// Verifies scenario where a request matches multiple filters.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn multiple_matching_filters(
    #[values(
        TestHttpKind::Http1,
        TestHttpKind::Http1Alpn,
        TestHttpKind::Http1NoAlpn,
        TestHttpKind::Http2,
        TestHttpKind::Http2Alpn,
        TestHttpKind::Http2NoAlpn
    )]
    http_kind: TestHttpKind,
) {
    // Initial setup
    let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_destination = original_server.local_addr().unwrap();
    let tls_setup = if http_kind.uses_tls() {
        Some(SimpleStore::new(original_destination.port(), &["h2", "http/1.1"]).await)
    } else {
        None
    };
    let (redirector, _redirector_state, mut conn_tx) = DummyRedirector::new();
    let (redirector, handle) = RedirectorTask::new(
        redirector,
        tls_setup
            .as_ref()
            .map(|setup| setup.store.clone())
            .unwrap_or_default(),
    );
    let (command_tx, command_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(command_rx, handle);
    tokio::spawn(redirector.run());
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer");

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: tls_setup.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: tls_setup.as_ref().map(SimpleStore::acceptor),
    };

    let clients = futures::stream::iter(0..3)
        .then(|id| {
            StealingClient::new(
                id,
                command_tx.clone(),
                "1.19.4",
                StealType::FilteredHttpEx(
                    original_destination.port(),
                    HttpFilter::Path(Filter::new("/api/v1".into()).unwrap()),
                ),
                stealer_status.clone(),
            )
        })
        .collect::<Vec<_>>()
        .await;

    tokio::spawn(async move {
        let conn = conn_tx.make_connection(original_destination).await;
        let mut sender = request.make_connection(conn).await;
        request.send(&mut sender, 0).await;
    });

    let mut logs = 0;
    let mut requests = 0;

    for mut client in clients {
        match client.recv().await {
            DaemonMessage::LogMessage(log) => {
                assert_eq!(log.level, LogLevel::Warn);
                assert!(log.message.contains("stolen by another user"));
                logs += 1;
            }
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(..)) => {
                requests += 1;
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    assert_eq!(logs, 2);
    assert_eq!(requests, 1);
}
