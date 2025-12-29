//! Tests for the whole [`steal`](super) module.
//!
//! Verify the whole flow from the [`TcpStealerApi`](super::api::TcpStealerApi)
//! to the [`RedirectorTask`](crate::incoming::RedirectorTask).
#![allow(clippy::indexing_slicing)]

use std::time::Duration;

use bytes::{Buf, Bytes};
use futures::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::SizeHint;
use mirrord_protocol::{
    DaemonMessage, LogLevel,
    tcp::{DaemonTcp, Filter, HttpBodyFilter, HttpFilter, IncomingTrafficTransportType, StealType},
};
use mirrord_tls_util::MaybeTls;
use rand::distr::{Alphanumeric, SampleString};
use rstest::rstest;
use rustls::pki_types::ServerName;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_rustls::TlsStream;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use utils::{StealingClient, TestBody, TestHttpKind, TestRequest, TestTcpProtocol, WithSizeHint};

use super::{StealerCommand, TcpStealerTask};
use crate::{
    incoming::{
        RedirectorTask, RedirectorTaskConfig,
        test::{DummyConnectionTx, DummyRedirector},
        tls::test::SimpleStore,
    },
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
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
    let mut setup = TestSetup::new_http(http_kind, RedirectorTaskConfig::from_env()).await;

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: if stolen { 0 } else { 1 },
        upgrade: Some(upgraded_protocol),
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: None,
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
    let mut setup = TestSetup::new_http(http_kind, RedirectorTaskConfig::from_env()).await;

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: None,
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
    let mut setup = TestSetup::new_tcp(with_tls, RedirectorTaskConfig::from_env()).await;

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
            let conn = match &setup.tls {
                Some(tls_setup) => {
                    let connector = tls_setup.connector(None);
                    let conn = connector
                        .connect(ServerName::try_from("server").unwrap(), conn)
                        .await
                        .unwrap();
                    MaybeTls::Tls(Box::new(TlsStream::Client(conn)))
                }
                _ => MaybeTls::NoTls(conn),
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
                let conn = match &setup.tls {
                    Some(tls_setup) => {
                        let acceptor = tls_setup.acceptor();
                        let conn = acceptor.accept(conn).await.unwrap();
                        MaybeTls::Tls(Box::new(TlsStream::Server(conn)))
                    }
                    _ => MaybeTls::NoTls(conn),
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
    let mut setup = TestSetup::new_tcp(true, RedirectorTaskConfig::from_env()).await;

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
    let mut setup = TestSetup::new_http(http_kind, RedirectorTaskConfig::from_env()).await;

    let request = TestRequest {
        path: "/api/v1".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: None,
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
    let mut setup = TestSetup::new_http(http_kind, RedirectorTaskConfig::from_env()).await;

    let mut requests = (0..4)
        .map(|i| TestRequest {
            path: "/api/v1".into(),
            id_header: i as usize,
            user_header: i,
            upgrade: None,
            kind: http_kind,
            connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
            acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
            body: None,
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
                        Filter::new(format!("{}: {id}", TestRequest::USER_ID_HEADER)).unwrap(),
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

/// Verifies that `Mirrord-Agent` headers are inserted with correct
/// values into responses to stolen http requests.
#[rstest]
#[tokio::test(flavor = "current_thread")]
#[timeout(Duration::from_secs(10))]
async fn header_injection(
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
    use crate::util::ClientId;

    let mut setup = TestSetup::new_http(
        http_kind,
        RedirectorTaskConfig {
            inject_headers: true,
        },
    )
    .await;

    let request_passthrough = TestRequest {
        path: "/passthrough".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: None,
    };

    let request_forwarded = TestRequest {
        path: "/forward".into(),
        id_header: 0,
        user_header: 0,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: None,
    };

    let mut client = StealingClient::new(
        1,
        setup.stealer_tx.clone(),
        "1.19.4",
        StealType::FilteredHttpEx(
            setup.original_server.local_addr().unwrap().port(),
            HttpFilter::Path(Filter::new("/forward".into()).unwrap()),
        ),
        setup.stealer_status.clone(),
    )
    .await;

    let mut run =
        async |request: &TestRequest, expected_header_value: &str, expect_handled_by: ClientId| {
            let conn = setup
                .conn_tx
                .make_connection(setup.original_server.local_addr().unwrap())
                .await;

            let mut sender = request.make_connection(conn).await;
            request
                .send_verify(&mut sender, expect_handled_by, |r| {
                    assert_eq!(
                        r.headers()
                            .get(TestRequest::MIRRORD_AGENT_HEADER)
                            .unwrap()
                            .to_str()
                            .unwrap(),
                        expected_header_value
                    )
                })
                .await;
        };

    tokio::join!(
        async {
            client.expect_request(&request_forwarded).await;
            let (stream, _) = setup.original_server.accept().await.unwrap();
            request_passthrough.accept(stream, 0).await;
        },
        async {
            run(&request_forwarded, "forwarded-to-client", 1).await;
            run(&request_passthrough, "passed-through", 0).await;
        }
    );
}

#[derive(Clone, Copy)]
enum FailReason {
    Timeout,
    TooBig,
}

/// Dictates the value of the content-length header
#[derive(Clone, Copy)]
enum SizeHintType {
    /// Remove the size hint, removing content-length header
    Missing,
    /// Apply size hint to the body, setting the content-length header
    Set,
    /// Keep the size hint of the original body
    Original,
}

impl SizeHintType {
    fn hint(self, real_size: u64) -> Option<SizeHint> {
        match self {
            SizeHintType::Missing => Some(SizeHint::default()),
            SizeHintType::Set => Some(SizeHint::with_exact(real_size)),
            SizeHintType::Original => None,
        }
    }
}

/// Verifies that when buffering the body fails (for whatever reason),
/// the request is correctly forwarded to the original destination
/// *unmodified*.
#[rstest]
#[tokio::test(flavor = "current_thread")]
#[timeout(Duration::from_secs(5))]
async fn body_filters_fail(
    #[values(
        TestHttpKind::Http1,
        TestHttpKind::Http1Alpn,
        TestHttpKind::Http1NoAlpn,
        TestHttpKind::Http2,
        TestHttpKind::Http2Alpn,
        TestHttpKind::Http2NoAlpn
    )]
    http_kind: TestHttpKind,

    #[values(FailReason::Timeout, FailReason::TooBig)] reason: FailReason,
    #[values(SizeHintType::Missing, SizeHintType::Set, SizeHintType::Original)]
    size_hint: SizeHintType,
) {
    use serde_json::json;

    let mut setup = TestSetup::new_http(http_kind, RedirectorTaskConfig::from_env()).await;

    let payload = Bytes::from(
        json!({
            "pass": true,
            "payload": Alphanumeric
            .sample_string(&mut rand::rng(), match reason {
                FailReason::Timeout => 48 * 1024,
                FailReason::TooBig => 128 * 1024,
            })
        })
        .to_string(),
    );

    let payload_2 = payload.clone();

    let request = TestRequest {
        path: "/".into(),
        id_header: 0,
        user_header: 1,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: Some(TestBody::new(
            move || {
                let mut initial = Full::new(payload.clone());
                let (tx, rx) = mpsc::channel(1);
                tokio::spawn(async move {
                    while let Some(frame) = initial.frame().await {
                        match (frame, reason) {
                            (Ok(frame), FailReason::Timeout) => {
                                let Some(data) = frame.data_ref() else {
                                    tx.send(Ok(frame))
                                        .await
                                        .expect("other end of channel closed");
                                    continue;
                                };
                                // Forcibly splitting frames to simulate a slow connection.
                                // This involves unnecessary copying but Bytes has no self-aware
                                // `chunk` and it's a test so it doesn't matter.
                                for chunk in data.chunks(1024) {
                                    tokio::time::sleep(Duration::from_millis(25)).await;
                                    tx.send(Ok(hyper::body::Frame::data(Bytes::copy_from_slice(
                                        chunk,
                                    ))))
                                    .await
                                    .expect("other end of channel closed");
                                }
                            }
                            (frame, _) => {
                                tx.send(frame).await.expect("other end of channel closed");
                            }
                        }
                    }
                });
                WithSizeHint::new(
                    StreamBody::new(ReceiverStream::new(rx)).map_err(|_| unreachable!()),
                    size_hint.hint(payload.len() as u64),
                )
            },
            move |_parts, mut body| {
                let mut remaining = payload_2.clone();
                Box::pin(async move {
                    while let Some(frame) = body.frame().await {
                        let frame = frame
                            .expect("invalid frame")
                            .into_data()
                            .expect("received non-data frame");

                        assert!(remaining.len() >= frame.len());
                        assert_eq!(&remaining[..frame.len()], &frame[..]);
                        remaining.advance(frame.len());
                    }
                    assert!(remaining.is_empty());
                })
            },
        )),
    };

    let _client = StealingClient::new(
        0,
        setup.stealer_tx.clone(),
        "1.22.1",
        StealType::FilteredHttpEx(
            setup.original_server.local_addr().unwrap().port(),
            HttpFilter::Body(HttpBodyFilter::Json {
                query: "$.pass".into(),
                matches: Filter::new("true".into()).unwrap(),
            }),
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
            let (stream, _) = setup.original_server.accept().await.unwrap();
            request.accept(stream, 1).await;
        },
        async {
            let mut sender = request.make_connection(conn).await;
            request.send(&mut sender, 1).await;
        }
    );
}

/// Verifies that when body filters work and correctly forward the
/// packets to the desired destination.
#[rstest]
#[tokio::test(flavor = "current_thread")]
#[timeout(Duration::from_secs(5))]
/// Verifies that the body gets buffered and passed to the clients correctly
async fn body_filters_pass(
    #[values(
        TestHttpKind::Http1,
        TestHttpKind::Http1Alpn,
        TestHttpKind::Http1NoAlpn,
        TestHttpKind::Http2,
        TestHttpKind::Http2Alpn,
        TestHttpKind::Http2NoAlpn
    )]
    http_kind: TestHttpKind,

    #[values(true, false)] match_filter: bool,
    #[values(12, 60 * 1024)] payload_len: usize,
    #[values(SizeHintType::Missing, SizeHintType::Set, SizeHintType::Original)]
    size_hint: SizeHintType,
) {
    use serde_json::json;

    let mut setup = TestSetup::new_http(http_kind, RedirectorTaskConfig::from_env()).await;

    let payload = Bytes::from(
        json!({
            "pass": match_filter,
            "payload": Alphanumeric
                .sample_string(&mut rand::rng(), payload_len)
        })
        .to_string(),
    );

    let payload_2 = payload.clone();

    let request = TestRequest {
        path: "/".into(),
        id_header: 0,
        user_header: 1,
        upgrade: None,
        kind: http_kind,
        connector: setup.tls.as_ref().map(|s| s.connector(http_kind.alpn())),
        acceptor: setup.tls.as_ref().map(SimpleStore::acceptor),
        body: Some(TestBody::new(
            move || {
                WithSizeHint::new(
                    Full::new(payload.clone()).map_err(|_| unreachable!()),
                    size_hint.hint(payload.len() as u64),
                )
            },
            move |_parts, mut body| {
                let mut remaining = payload_2.clone();
                Box::pin(async move {
                    while let Some(frame) = { body.frame().await } {
                        let frame = frame
                            .expect("invalid frame")
                            .into_data()
                            .expect("received non-data frame");

                        assert!(remaining.len() >= frame.len());
                        assert_eq!(&remaining[..frame.len()], &frame[..]);
                        remaining.advance(frame.len());
                    }
                    assert!(remaining.is_empty());
                })
            },
        )),
    };

    let mut client = StealingClient::new(
        0,
        setup.stealer_tx.clone(),
        "1.22.1",
        StealType::FilteredHttpEx(
            setup.original_server.local_addr().unwrap().port(),
            HttpFilter::Body(HttpBodyFilter::Json {
                query: "$.pass".into(),
                matches: Filter::new("rue".into()).unwrap(),
            }),
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
            if match_filter {
                client.expect_request(&request).await;
            } else {
                let (stream, _) = setup.original_server.accept().await.unwrap();
                request.accept(stream, 1).await;
            }
        },
        async {
            let mut sender = request.make_connection(conn).await;
            request
                .send(&mut sender, if match_filter { 0 } else { 1 })
                .await;
        }
    );
}

struct TestSetup {
    /// Simulates the app that would be running on the cluster.
    original_server: TcpListener,
    stealer_tx: mpsc::Sender<StealerCommand>,
    stealer_status: BgTaskStatus,
    tls: Option<SimpleStore>,
    conn_tx: DummyConnectionTx,
    _runtime: BgTaskRuntime,
}

impl TestSetup {
    async fn new_http(http_kind: TestHttpKind, redirector_config: RedirectorTaskConfig) -> Self {
        Self::new_tcp(http_kind.uses_tls(), redirector_config).await
    }

    async fn new_tcp(with_tls: bool, redirector_config: RedirectorTaskConfig) -> Self {
        let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let original_destination = original_server.local_addr().unwrap();
        let tls_setup = if with_tls {
            Some(SimpleStore::new(original_destination.port(), &["h2", "http/1.1"]).await)
        } else {
            None
        };
        let (redirector, _redirector_state, conn_tx) = DummyRedirector::new();
        let (redirector, handle, _) = RedirectorTask::new(
            redirector,
            tls_setup
                .as_ref()
                .map(|setup| setup.store.clone())
                .unwrap_or_default(),
            redirector_config,
        );
        let (stealer_tx, stealer_rx) = mpsc::channel(8);
        let stealer_task = TcpStealerTask::new(stealer_rx, handle);
        tokio::spawn(redirector.run());

        let local_bg_task_runtime = BgTaskRuntime::spawn(None).await.unwrap();
        let stealer_status = local_bg_task_runtime
            .handle()
            .spawn(stealer_task.run(CancellationToken::new()))
            .into_status("stealer");

        Self {
            original_server,
            stealer_tx,
            stealer_status,
            tls: tls_setup,
            conn_tx,
            _runtime: local_bg_task_runtime,
        }
    }
}
