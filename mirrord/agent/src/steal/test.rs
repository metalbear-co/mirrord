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

use super::{StealerCommand, TcpStealerTask};
use crate::{
    incoming::{
        test::{DummyConnectionTx, DummyRedirector},
        tls::test::SimpleStore,
        RedirectorTask,
    },
    util::remote_runtime::{BgTaskRuntime, BgTaskStatus, IntoStatus},
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
    let mut setup = TestSetup::new_http(http_kind).await;

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: if stolen { 0 } else { 1 },
        upgrade: Some(upgraded_protocol),
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
    };

    let mut stealing_client = StealingClient::new(
        0,
        setup.stealer_tx.clone(),
        "1.19.4",
        StealType::FilteredHttpEx(
            setup.original_server.local_addr().unwrap().port(),
            HttpFilter::Header(Filter::new(format!("{}: 0", TestRequest::USER_ID_HEADER)).unwrap()),
        ),
        setup.stealer_status.clone(),
    )
    .await;
    let conn = setup
        .conn_tx
        .make_connection(setup.original_server.local_addr().unwrap())
        .await;

    tokio::join!(
        async {
            let mut sender = request.make_connection(conn).await;
            request.send(&mut sender, if stolen { 0 } else { 1 }).await;
        },
        async {
            if stolen {
                stealing_client.expect_request(&request).await;
            } else {
                let (stream, _) = setup.original_server.accept().await.unwrap();
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
    let mut setup = TestSetup::new_http(http_kind).await;

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
    };

    let mut client_1 = StealingClient::new(
        0,
        setup.stealer_tx.clone(),
        "1.19.3", // not high enough for stealing requests with unfiltered subscription
        StealType::All(setup.original_server.local_addr().unwrap().port()),
        setup.stealer_status.clone(),
    )
    .await;
    let conn = setup
        .conn_tx
        .make_connection(setup.original_server.local_addr().unwrap())
        .await;
    let mut sender = request.make_connection(conn).await;
    tokio::join!(
        request.send(&mut sender, 2137),
        async {
            let (stream, _) = setup.original_server.accept().await.unwrap();
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
        setup.stealer_tx.clone(),
        "1.19.4", // high enough for stealing requests with unfiltered subscription
        StealType::All(setup.original_server.local_addr().unwrap().port()),
        setup.stealer_status.clone(),
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
    let mut setup = TestSetup::new_tcp(with_tls).await;

    let steal_type = if stolen {
        StealType::All(setup.original_server.local_addr().unwrap().port())
    } else {
        StealType::FilteredHttpEx(
            setup.original_server.local_addr().unwrap().port(),
            HttpFilter::Header(Filter::new("whatever".into()).unwrap()),
        )
    };
    let mut client = StealingClient::new(
        0,
        setup.stealer_tx.clone(),
        "1.19.4",
        steal_type,
        setup.stealer_status.clone(),
    )
    .await;

    let conn = setup
        .conn_tx
        .make_connection(setup.original_server.local_addr().unwrap())
        .await;
    tokio::join!(
        async {
            let conn = if let Some(tls_setup) = &setup.tls {
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
                let (conn, _) = setup.original_server.accept().await.unwrap();
                let conn = if let Some(tls_setup) = &setup.tls {
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
    let mut setup = TestSetup::new_tcp(true).await;

    let mut client = StealingClient::new(
        0,
        setup.stealer_tx.clone(),
        "1.19.3", // not high enough for stealing TLS connections
        StealType::All(setup.original_server.local_addr().unwrap().port()),
        setup.stealer_status.clone(),
    )
    .await;

    let conn = setup
        .conn_tx
        .make_connection(setup.original_server.local_addr().unwrap())
        .await;
    tokio::join!(
        async {
            let connector = setup.tls.as_ref().unwrap().connector(None);
            let conn = connector
                .connect(ServerName::try_from("server").unwrap(), conn)
                .await
                .unwrap();
            TestTcpProtocol::Echo.run(conn, false).await;
        },
        async {
            let (conn, _) = setup.original_server.accept().await.unwrap();
            let acceptor = setup.tls.as_ref().unwrap().acceptor();
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
    let mut setup = TestSetup::new_http(http_kind).await;

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
    };

    let clients = futures::stream::iter(0..3)
        .then(|id| {
            StealingClient::new(
                id,
                setup.stealer_tx.clone(),
                "1.19.4",
                StealType::FilteredHttpEx(
                    setup.original_server.local_addr().unwrap().port(),
                    HttpFilter::Path(Filter::new("/api/v1".into()).unwrap()),
                ),
                setup.stealer_status.clone(),
            )
        })
        .collect::<Vec<_>>()
        .await;

    tokio::spawn(async move {
        let conn = setup
            .conn_tx
            .make_connection(setup.original_server.local_addr().unwrap())
            .await;
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

/// Verifies scenario where we have multiple filtered subscriptions.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn multiple_filtered_subscriptions(
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
    let mut setup = TestSetup::new_http(http_kind).await;

    let mut requests = (0..4)
        .map(|i| TestRequest {
            path: "/api/v1".into(),
            id_header: i as usize,
            user_header: i,
            upgrade: None,
            kind: http_kind,
            connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
            acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        })
        .collect::<Vec<_>>();
    let last_request = requests.pop().unwrap();

    let mut clients = futures::stream::iter(0..3)
        .then(|id| {
            StealingClient::new(
                id,
                setup.stealer_tx.clone(),
                "1.19.4",
                StealType::FilteredHttpEx(
                    setup.original_server.local_addr().unwrap().port(),
                    HttpFilter::Header(
                        Filter::new(format!("{}: {id}", TestRequest::USER_ID_HEADER).into())
                            .unwrap(),
                    ),
                ),
                setup.stealer_status.clone(),
            )
        })
        .collect::<Vec<_>>()
        .await;

    tokio::join!(
        async {
            let conn = setup
                .conn_tx
                .make_connection(setup.original_server.local_addr().unwrap())
                .await;
            let mut sender = last_request.make_connection(conn).await;
            for request in &requests {
                request.send(&mut sender, request.user_header).await;
            }
            last_request
                .send(&mut sender, last_request.user_header)
                .await;
        },
        async {
            for (client, request) in clients.iter_mut().zip(&requests) {
                client.expect_request(request).await;
            }
        },
        async {
            let (stream, _) = setup.original_server.accept().await.unwrap();
            last_request.accept(stream, last_request.user_header).await;
        }
    );
}

struct TestSetup {
    original_server: TcpListener,
    stealer_tx: mpsc::Sender<StealerCommand>,
    stealer_status: BgTaskStatus,
    tls: Option<SimpleStore>,
    conn_tx: DummyConnectionTx,
}

impl TestSetup {
    async fn new_http(http_kind: TestHttpKind) -> Self {
        Self::new_tcp(http_kind.uses_tls()).await
    }

    async fn new_tcp(with_tls: bool) -> Self {
        let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let original_destination = original_server.local_addr().unwrap();
        let tls_setup = if with_tls {
            Some(SimpleStore::new(original_destination.port(), &["h2", "http/1.1"]).await)
        } else {
            None
        };
        let (redirector, _redirector_state, conn_tx) = DummyRedirector::new();
        let (redirector, handle) = RedirectorTask::new(
            redirector,
            tls_setup
                .as_ref()
                .map(|setup| setup.store.clone())
                .unwrap_or_default(),
        );
        let (stealer_tx, stealer_rx) = mpsc::channel(8);
        let stealer_task = TcpStealerTask::new(stealer_rx, handle);
        tokio::spawn(redirector.run());
        let stealer_status = BgTaskRuntime::Local
            .spawn(stealer_task.run())
            .into_status("stealer");

        Self {
            original_server,
            stealer_tx,
            stealer_status,
            tls: tls_setup,
            conn_tx,
        }
    }
}
