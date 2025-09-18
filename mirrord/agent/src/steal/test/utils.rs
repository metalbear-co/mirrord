use std::{
    fmt,
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    time::Duration,
};

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Empty, StreamBody, combinators::BoxBody};
use hyper::{
    Request, Response,
    body::{Frame, Incoming},
    header,
    http::{HeaderName, Method, StatusCode, request},
};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogLevel,
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, ChunkedResponse, DaemonTcp,
        HTTP_CHUNKED_REQUEST_V2_VERSION, HTTP_CHUNKED_RESPONSE_VERSION, HttpRequestMetadata,
        HttpResponse, IncomingTrafficTransportType, InternalHttpBodyNew, InternalHttpRequest,
        InternalHttpResponse, LayerTcpSteal, MODE_AGNOSTIC_HTTP_REQUESTS, NewTcpConnectionV2,
        StealType, TcpClose, TcpData,
    },
};
use mirrord_tls_util::MaybeTls;
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc::{self, Sender},
};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    http::{
        HttpVersion, body::RolledBackBody, extract_requests::ExtractedRequests, sender::HttpSender,
    },
    steal::{StealerCommand, TcpStealerApi},
    util::{ClientId, protocol_version::ClientProtocolVersion, remote_runtime::BgTaskStatus},
};

/// TCP protocols used in steal tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestTcpProtocol {
    /// Client:
    /// 1. 3 times sends `hello\n`, expecting each message to be echoed back.
    /// 2. Shuts down writing.
    /// 3. Waits for write shutdown from the server.
    ///
    /// Server:
    /// 1. Reads all data from the client and echoes it back.
    /// 2. Shuts down writing.
    Echo,
    /// Client:
    /// 1. Waits for the server to shut down writing.
    /// 2. 3 times sends `hello\n`.
    /// 3. Shuts down writing.
    ///
    /// Server:
    /// 1. Shuts down writing.
    /// 2. Reads all data from the client.
    ClientTalks,
    /// Like [`TcpProtocolKind::ClientTalks`] but reversed.
    ServerTalks,
}

impl TestTcpProtocol {
    const MESSAGE: &[u8] = b"hello\n";

    pub fn name(self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::ClientTalks => "client-talks",
            Self::ServerTalks => "server-talks",
        }
    }

    /// Runs this protocol on the given IO stream.
    pub async fn run<IO>(self, mut io: IO, is_server: bool)
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let mut buf = vec![0_u8; Self::MESSAGE.len()];

        match (self, is_server) {
            (Self::Echo, true) => {
                for _ in 0..3 {
                    io.read_exact(&mut buf).await.unwrap();
                    assert_eq!(&buf, Self::MESSAGE);
                    io.write_all(&buf).await.unwrap();
                    io.flush().await.unwrap();
                }
                assert_eq!(io.read(&mut buf).await.unwrap(), 0);
                io.shutdown().await.unwrap();
            }

            (Self::Echo, false) => {
                for _ in 0..3 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    io.write_all(Self::MESSAGE).await.unwrap();
                    io.read_exact(&mut buf).await.unwrap();
                    assert_eq!(&buf, Self::MESSAGE);
                }
                io.shutdown().await.unwrap();
                assert_eq!(io.read(&mut buf).await.unwrap(), 0);
            }

            (Self::ClientTalks, false) | (Self::ServerTalks, true) => {
                assert_eq!(io.read(&mut buf).await.unwrap(), 0);
                for _ in 0..3 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    io.write_all(Self::MESSAGE).await.unwrap();
                    io.flush().await.unwrap();
                }
                io.shutdown().await.unwrap();
            }

            (Self::ClientTalks, true) | (Self::ServerTalks, false) => {
                io.shutdown().await.unwrap();
                for _ in 0..3 {
                    io.read_exact(&mut buf).await.unwrap();
                    assert_eq!(&buf, Self::MESSAGE);
                }
                assert_eq!(io.read(&mut buf).await.unwrap(), 0);
            }
        }
    }
}

/// HTTP connections used in steal tests.
#[derive(Clone, Copy, Debug)]
pub enum TestHttpKind {
    /// Plain HTTP/1.1.
    Http1,
    /// Plain HTTP/2.
    Http2,
    /// HTTP/1.1 over TLS, with no ALPN upgrade.
    Http1NoAlpn,
    /// HTTP/2 over TLS, with no ALPN upgrade.
    Http2NoAlpn,
    /// HTTP/1.1 over TLS, with an ALPN upgrade.
    Http1Alpn,
    /// HTTP/2 over TLS, with an ALPN upgrade.
    Http2Alpn,
}

impl TestHttpKind {
    pub fn uses_tls(self) -> bool {
        matches!(self, Self::Http1 | Self::Http2).not()
    }

    pub fn alpn(self) -> Option<&'static str> {
        match self {
            Self::Http1Alpn => Some("http/1.1"),
            Self::Http2Alpn => Some("h2"),
            _ => None,
        }
    }

    pub fn version(self) -> HttpVersion {
        match self {
            Self::Http1 | Self::Http1Alpn | Self::Http1NoAlpn => HttpVersion::V1,
            Self::Http2 | Self::Http2Alpn | Self::Http2NoAlpn => HttpVersion::V2,
        }
    }
}

/// HTTP request used in steal tests.
#[derive(Clone)]
pub struct TestRequest {
    pub path: String,
    pub id_header: usize,
    pub user_header: ClientId,
    pub upgrade: Option<TestTcpProtocol>,
    pub kind: TestHttpKind,
    pub connector: Option<TlsConnector>,
    pub acceptor: Option<TlsAcceptor>,
}

impl fmt::Debug for TestRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestRequest")
            .field("path", &self.path)
            .field("id_header", &self.id_header)
            .field("user_header", &self.user_header)
            .field("upgrade", &self.upgrade)
            .field("kind", &self.kind)
            .finish()
    }
}

impl TestRequest {
    const FRAME: &[u8] = b"hello\n";
    const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("request-id");
    pub const USER_ID_HEADER: HeaderName = HeaderName::from_static("user-id");
    const HANDLED_BY_HEADER: HeaderName = HeaderName::from_static("handled-by");

    fn as_hyper_request(&self) -> Request<BoxBody<Bytes, hyper::Error>> {
        let uri = format!(
            "{}://server{}",
            if self.kind.uses_tls() {
                "https"
            } else {
                "http"
            },
            self.path,
        );
        match self.upgrade {
            Some(protocol) => Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, protocol.name())
                .header(Self::REQUEST_ID_HEADER, self.id_header.to_string())
                .header(Self::USER_ID_HEADER, self.user_header.to_string())
                .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
                .unwrap(),
            None => {
                let (frame_tx, frame_rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                tokio::spawn(async move {
                    for _ in 0..3 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        frame_tx
                            .send(Ok(Frame::data(Self::FRAME.into())))
                            .await
                            .unwrap();
                    }
                });
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(Self::REQUEST_ID_HEADER, self.id_header.to_string())
                    .header(Self::USER_ID_HEADER, self.user_header.to_string())
                    .body(BoxBody::new(StreamBody::new(ReceiverStream::new(frame_rx))))
                    .unwrap()
            }
        }
    }

    /// Generates a test response to this request. Includes
    /// `request-id` and `handled-by` headers for running later
    /// assertions.
    fn as_hyper_response(&self, handled_by: ClientId) -> Response<BoxBody<Bytes, hyper::Error>> {
        match self.upgrade {
            Some(protocol) => Response::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, protocol.name())
                .header(Self::REQUEST_ID_HEADER, self.id_header.to_string())
                .header(Self::HANDLED_BY_HEADER, handled_by.to_string())
                .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
                .unwrap(),
            None => {
                let (frame_tx, frame_rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                tokio::spawn(async move {
                    for _ in 0..3 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        frame_tx
                            .send(Ok(Frame::data(Self::FRAME.into())))
                            .await
                            .unwrap();
                    }
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(Self::REQUEST_ID_HEADER, self.id_header.to_string())
                    .header(Self::HANDLED_BY_HEADER, handled_by.to_string())
                    .body(BoxBody::new(StreamBody::new(ReceiverStream::new(frame_rx))))
                    .unwrap()
            }
        }
    }

    async fn verify_request(&self, parts: request::Parts, mut body: RolledBackBody) {
        assert_eq!(
            parts
                .headers
                .get(Self::REQUEST_ID_HEADER)
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            self.id_header,
        );
        assert_eq!(
            parts
                .headers
                .get(Self::USER_ID_HEADER)
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<ClientId>()
                .unwrap(),
            self.user_header,
        );
        assert_eq!(parts.uri.path(), self.path);

        match self.upgrade {
            Some(protocol) => {
                assert_eq!(parts.method, Method::GET);
                assert!(body.frame().await.is_none());
                assert_eq!(
                    parts
                        .headers
                        .get(header::CONNECTION)
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    "upgrade"
                );
                assert_eq!(
                    parts
                        .headers
                        .get(header::UPGRADE)
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    protocol.name()
                );
            }
            None => {
                assert_eq!(parts.method, Method::POST);
                let body = body.collect().await.unwrap().to_bytes();
                let expected = std::iter::repeat_n(Self::FRAME, 3)
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>();
                assert_eq!(body.as_ref(), &expected);
            }
        }
    }

    async fn verify_response(&self, mut response: Response<Incoming>, expect_handled_by: ClientId) {
        assert_eq!(
            response
                .headers()
                .get(Self::REQUEST_ID_HEADER)
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            self.id_header,
        );
        assert_eq!(
            response
                .headers()
                .get(Self::HANDLED_BY_HEADER)
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<ClientId>()
                .unwrap(),
            expect_handled_by,
        );

        match self.upgrade {
            Some(protocol) => {
                assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
                assert!(response.body_mut().frame().await.is_none());
                assert_eq!(
                    response
                        .headers()
                        .get(header::CONNECTION)
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    "upgrade"
                );
                assert_eq!(
                    response
                        .headers()
                        .get(header::UPGRADE)
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    protocol.name()
                );
            }
            None => {
                assert_eq!(response.status(), StatusCode::OK);
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let expected = std::iter::repeat_n(Self::FRAME, 3)
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>();
                assert_eq!(
                    body.as_ref(),
                    &expected,
                    "unexpected body: {}",
                    String::from_utf8_lossy(&body)
                );
            }
        }
    }

    /// Accept a request on `conn`, verify that it matches this one,
    /// and respond to it.
    ///
    /// This is only useful for verifying passthrough requests. For
    /// stolen requests, see [`StealingClient::expect_request`].
    pub async fn accept(&self, conn: TcpStream, handled_by: ClientId) {
        let conn = match &self.acceptor {
            Some(acceptor) => {
                let conn = acceptor.accept(conn).await.unwrap();

                if let Some(alpn) = self.kind.alpn() {
                    assert_eq!(conn.get_ref().1.alpn_protocol(), Some(alpn.as_bytes()),);
                }

                println!("[{}:{}] Accepted a TLS connection", file!(), line!());
                MaybeTls::Tls(Box::new(TlsStream::Server(conn)))
            }
            None => MaybeTls::NoTls(conn),
        };
        let mut requests = ExtractedRequests::new(TokioIo::new(conn), self.kind.version());
        let request = requests.next().await.unwrap().unwrap();
        let conn_task = tokio::spawn(async move { requests.next().await });

        println!(
            "[{}:{}] Got request: {request:?}, expected {self:?}",
            file!(),
            line!()
        );

        self.verify_request(
            request.parts,
            RolledBackBody {
                head: request.body_head.into_iter(),
                tail: request.body_tail,
            },
        )
        .await;
        println!("[{}:{}] Request ok, sending response", file!(), line!());
        let response = self.as_hyper_response(handled_by);
        request.response_tx.send(response).unwrap();

        if let Some(protocol) = self.upgrade {
            println!(
                "[{}:{}] Processing upgrade to {protocol:?}",
                file!(),
                line!()
            );
            let upgraded = request.upgrade.await.unwrap();
            protocol.run(TokioIo::new(upgraded), true).await;
        }

        assert!(conn_task.await.unwrap().is_none());
    }

    pub async fn make_connection(
        &self,
        conn: TcpStream,
    ) -> HttpSender<BoxBody<Bytes, hyper::Error>> {
        let conn = match &self.connector {
            Some(connector) => {
                if let Some(alpn) = self.kind.alpn() {
                    assert!(
                        connector
                            .config()
                            .alpn_protocols
                            .contains(&alpn.as_bytes().to_vec()),
                        "bug in test code"
                    );
                }
                let conn = connector
                    .connect(ServerName::try_from("server").unwrap(), conn)
                    .await
                    .unwrap();
                MaybeTls::Tls(Box::new(TlsStream::Client(conn)))
            }
            None => MaybeTls::NoTls(conn),
        };
        let sender = HttpSender::new(TokioIo::new(conn), self.kind.version())
            .await
            .unwrap();
        println!(
            "[{}:{}] HTTP client connected for request {self:?}",
            file!(),
            line!()
        );

        sender
    }

    pub async fn send(
        &self,
        sender: &mut HttpSender<BoxBody<Bytes, hyper::Error>>,
        expect_handled_by: ClientId,
    ) {
        println!("[{}:{}] Sending request: {self:?}", file!(), line!());
        let request = self.as_hyper_request();
        let mut response = sender.send(request).await.unwrap();

        println!(
            "[{}:{}] Got response: {response:?}, expected {self:?}",
            file!(),
            line!()
        );
        let upgrade = hyper::upgrade::on(&mut response);
        self.verify_response(response, expect_handled_by).await;
        println!("[{}:{}] Response ok", file!(), line!());

        if let Some(protocol) = self.upgrade {
            println!(
                "[{}:{}] Processing upgrade to {protocol:?}",
                file!(),
                line!()
            );
            protocol
                .run(TokioIo::new(upgrade.await.unwrap()), false)
                .await;
        }
    }
}

/// Client for listening for and handling stolen connections. Directly
/// receives and interprets `DaemonMessage`s. For handling passthrough
/// requests, see `TestRequest::accept` and friends.
pub struct StealingClient {
    id: ClientId,
    api: TcpStealerApi,
    protocol_version: ClientProtocolVersion,
    steal_type: StealType,
}

impl StealingClient {
    /// Creates a new [`StealingClient`], subscribed with the given [`StealType`].
    ///
    /// # Protocol version
    ///
    /// This function will panic if called with protocol version not matchind
    /// [`HTTP_CHUNKED_RESPONSE_VERSION`]. Full support for chunked HTTP was added a long time ago.
    /// There would be very little benefit in adding more test code to cover older CLIs.
    pub async fn new(
        id: ClientId,
        command_tx: Sender<StealerCommand>,
        protocol_version: &str,
        steal_type: StealType,
        stealer_status: BgTaskStatus,
    ) -> Self {
        let protocol_version = protocol_version.parse::<ClientProtocolVersion>().unwrap();
        assert!(protocol_version.matches(&HTTP_CHUNKED_RESPONSE_VERSION));

        let mut api = TcpStealerApi::new(id, protocol_version.clone(), command_tx, stealer_status)
            .await
            .unwrap();
        api.handle_client_message(LayerTcpSteal::PortSubscribe(steal_type.clone()))
            .await
            .unwrap();
        assert_eq!(
            api.recv().await.unwrap(),
            DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(steal_type.get_port()))),
        );

        Self {
            id,
            api,
            protocol_version,
            steal_type,
        }
    }

    pub async fn expect_log(&mut self, level: LogLevel, containing: &str) {
        match self.api.recv().await.unwrap() {
            DaemonMessage::LogMessage(log) => {
                assert_eq!(log.level, level);
                assert!(log.message.contains(containing));
                println!(
                    "[{}:{}] Client {} got log: {log:?}",
                    file!(),
                    line!(),
                    self.id
                );
            }
            other => panic!(
                "client {} received an unexpected message:{other:?}",
                self.id
            ),
        }
    }

    pub async fn expect_connection(&mut self) -> NewTcpConnectionV2 {
        match self.api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::NewConnectionV1(conn)) => {
                assert!(
                    self.protocol_version
                        .matches(&MODE_AGNOSTIC_HTTP_REQUESTS)
                        .not()
                );
                NewTcpConnectionV2 {
                    connection: conn,
                    transport: IncomingTrafficTransportType::Tcp,
                }
            }

            DaemonMessage::TcpSteal(DaemonTcp::NewConnectionV2(conn)) => {
                assert!(self.protocol_version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS));
                conn
            }

            other => panic!("unexpected message: {other:?}"),
        }
    }

    /// For handling passthrough requests, see [`TestRequest::accept`].
    pub async fn expect_request(&mut self, expected: &TestRequest) {
        let mut request = match self.api.recv().await.unwrap() {
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV1(
                request,
            ))) => {
                println!(
                    "[{}:{}] Got request: {request:?}, expected {expected:?}",
                    file!(),
                    line!(),
                );

                assert!(
                    self.protocol_version
                        .matches(&HTTP_CHUNKED_REQUEST_V2_VERSION)
                        .not()
                );
                assert_eq!(request.port, self.steal_type.get_port(),);
                ChunkedRequestStartV2 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    metadata: HttpRequestMetadata::V1 {
                        source: SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
                        destination: SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), request.port),
                    },
                    request: InternalHttpRequest {
                        version: request.internal_request.version,
                        method: request.internal_request.method,
                        uri: request.internal_request.uri,
                        headers: request.internal_request.headers,
                        body: InternalHttpBodyNew {
                            frames: request.internal_request.body,
                            is_last: false,
                        },
                    },
                    transport: IncomingTrafficTransportType::Tcp,
                }
            }
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(
                request,
            ))) => {
                println!(
                    "[{}:{}] Got request: {request:?}, expected {expected:?}",
                    file!(),
                    line!(),
                );
                assert!(
                    self.protocol_version
                        .matches(&HTTP_CHUNKED_REQUEST_V2_VERSION)
                );
                let HttpRequestMetadata::V1 { destination, .. } = &request.metadata;
                assert_eq!(destination.port(), self.steal_type.get_port(),);
                request
            }
            other => panic!(
                "client {} received an unexpected message: {other:?}",
                self.id
            ),
        };

        match request.transport {
            IncomingTrafficTransportType::Tcp => {
                assert!(expected.kind.uses_tls().not());
            }
            IncomingTrafficTransportType::Tls { alpn_protocol, .. } => {
                assert_eq!(
                    alpn_protocol,
                    expected.kind.alpn().map(|alpn| alpn.as_bytes().to_vec())
                );
            }
        }

        while request.request.body.is_last.not() {
            match self.api.recv().await.unwrap() {
                DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(
                    ChunkedRequestBodyV1 {
                        frames,
                        is_last,
                        request_id,
                        connection_id,
                    },
                ))) => {
                    assert_eq!(request_id, request.request_id);
                    assert_eq!(connection_id, request.connection_id);
                    request.request.body.frames.extend(frames);
                    request.request.body.is_last = is_last;
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
        let body = RolledBackBody {
            head: request
                .request
                .body
                .frames
                .into_iter()
                .map(From::from)
                .collect::<Vec<_>>()
                .into_iter(),
            tail: None,
        };
        let mut hyper_request = Request::new(body);
        *hyper_request.method_mut() = request.request.method;
        *hyper_request.uri_mut() = request.request.uri;
        *hyper_request.version_mut() = request.request.version;
        *hyper_request.headers_mut() = request.request.headers;
        let (parts, body) = hyper_request.into_parts();
        expected.verify_request(parts, body).await;
        println!("[{}:{}] Request ok, sending response", file!(), line!());

        let (parts, mut body) = expected.as_hyper_response(self.id).into_parts();
        let response = ChunkedResponse::Start(HttpResponse {
            port: self.steal_type.get_port(),
            connection_id: request.connection_id,
            request_id: request.request_id,
            internal_response: InternalHttpResponse {
                status: parts.status,
                version: parts.version,
                headers: parts.headers,
                body: Default::default(),
            },
        });
        self.api
            .handle_client_message(LayerTcpSteal::HttpResponseChunked(response))
            .await
            .unwrap();
        while let Some(frame) = body.frame().await.transpose().unwrap() {
            self.api
                .handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                    ChunkedRequestBodyV1 {
                        frames: vec![frame.into()],
                        is_last: false,
                        request_id: request.request_id,
                        connection_id: request.connection_id,
                    },
                )))
                .await
                .unwrap();
        }
        self.api
            .handle_client_message(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                ChunkedRequestBodyV1 {
                    frames: vec![],
                    is_last: true,
                    request_id: request.request_id,
                    connection_id: request.connection_id,
                },
            )))
            .await
            .unwrap();

        if let Some(protocol) = expected.upgrade {
            println!(
                "[{}:{}] Processing upgrade to {protocol:?}",
                file!(),
                line!()
            );
            self.expect_tcp(request.connection_id, protocol).await;
        }
    }

    pub async fn expect_tcp(
        &mut self,
        expect_connection_id: ConnectionId,
        protocol: TestTcpProtocol,
    ) {
        let expected = std::iter::repeat_n(TestTcpProtocol::MESSAGE, 3)
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let mut got_bytes = vec![];

        match protocol {
            TestTcpProtocol::Echo => {
                while got_bytes != expected {
                    let bytes = match self.api.recv().await.unwrap() {
                        DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                            bytes,
                            connection_id,
                        })) => {
                            assert_eq!(connection_id, expect_connection_id);
                            bytes
                        }
                        other => panic!("unexpected message: {other:?}"),
                    };
                    got_bytes.extend_from_slice(&bytes);
                    assert!(expected.starts_with(&got_bytes));
                    self.api
                        .handle_client_message(LayerTcpSteal::Data(TcpData {
                            connection_id: expect_connection_id,
                            bytes,
                        }))
                        .await
                        .unwrap();
                }

                match self.api.recv().await.unwrap() {
                    DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                        bytes,
                        connection_id,
                    })) => {
                        assert_eq!(connection_id, expect_connection_id);
                        assert!(bytes.is_empty());
                    }
                    other => panic!("unexpected message: {other:?}"),
                };

                self.api
                    .handle_client_message(LayerTcpSteal::Data(TcpData {
                        connection_id: expect_connection_id,
                        bytes: Default::default(),
                    }))
                    .await
                    .unwrap();

                assert_eq!(
                    self.api.recv().await.unwrap(),
                    DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
                        connection_id: expect_connection_id
                    })),
                );
            }

            TestTcpProtocol::ClientTalks => {
                self.api
                    .handle_client_message(LayerTcpSteal::Data(TcpData {
                        connection_id: expect_connection_id,
                        bytes: Default::default(),
                    }))
                    .await
                    .unwrap();

                while expected != got_bytes {
                    match self.api.recv().await.unwrap() {
                        DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                            bytes,
                            connection_id,
                        })) => {
                            assert_eq!(connection_id, expect_connection_id);
                            got_bytes.extend(bytes.as_ref());
                            assert!(expected.starts_with(&got_bytes));
                        }
                        other => panic!("unexpected message: {other:?}"),
                    }
                }

                match self.api.recv().await.unwrap() {
                    DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                        bytes,
                        connection_id,
                    })) => {
                        assert_eq!(connection_id, expect_connection_id);
                        assert!(bytes.is_empty());
                    }
                    other => panic!("unexpected message: {other:?}"),
                };

                assert_eq!(
                    self.api.recv().await.unwrap(),
                    DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
                        connection_id: expect_connection_id
                    })),
                );
            }

            TestTcpProtocol::ServerTalks => {
                assert_eq!(
                    self.api.recv().await.unwrap(),
                    DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                        connection_id: expect_connection_id,
                        bytes: Default::default(),
                    })),
                );

                for _ in 0..3 {
                    self.api
                        .handle_client_message(LayerTcpSteal::Data(TcpData {
                            connection_id: expect_connection_id,
                            bytes: TestTcpProtocol::MESSAGE.into(),
                        }))
                        .await
                        .unwrap();
                }

                self.api
                    .handle_client_message(LayerTcpSteal::Data(TcpData {
                        connection_id: expect_connection_id,
                        bytes: Default::default(),
                    }))
                    .await
                    .unwrap();

                assert_eq!(
                    self.api.recv().await.unwrap(),
                    DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
                        connection_id: expect_connection_id
                    })),
                );
            }
        }
    }

    pub async fn recv(&mut self) -> DaemonMessage {
        self.api.recv().await.unwrap()
    }
}
