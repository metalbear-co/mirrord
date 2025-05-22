use std::ops::Not;

use bytes::Bytes;
use futures::StreamExt;
use http::HeaderValue;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, StreamBody};
use hyper::{
    body::Frame,
    header,
    http::{request, response, Method, StatusCode},
    Request, Response,
};
use hyper_util::rt::TokioIo;
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, TlsStream};
use tokio_stream::wrappers::ReceiverStream;

use crate::http::{
    body::RolledBackBody,
    extract_requests::{ExtractedRequest, ExtractedRequests},
    sender::HttpSender,
    HttpVersion,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpProtocolKind {
    Echo,
    ClientTalks,
    ServerTalks,
}

impl TcpProtocolKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::ClientTalks => "client-talks",
            Self::ServerTalks => "server-talks",
        }
    }

    pub fn from_request(parts: &request::Parts) -> Option<Self> {
        if parts.method != Method::GET {
            return None;
        }

        let connection = parts.headers.get(header::CONNECTION)?;
        if connection.to_str().unwrap() != "upgrade" {
            return None;
        }

        let upgrade = parts.headers.get(header::UPGRADE)?;
        match upgrade.to_str().unwrap() {
            "echo" => Some(Self::Echo),
            "client-talks" => Some(Self::ClientTalks),
            "server-talks" => Some(Self::ServerTalks),
            _ => None,
        }
    }

    pub fn from_response(parts: &response::Parts) -> Self {
        assert_eq!(parts.status, StatusCode::SWITCHING_PROTOCOLS);
        let connection = parts.headers.get(header::CONNECTION).unwrap();
        assert_eq!(connection.to_str().unwrap(), "upgrade");
        let upgrade = parts.headers.get(header::UPGRADE).unwrap();
        match upgrade.to_str().unwrap() {
            "echo" => Self::Echo,
            "client-talks" => Self::ClientTalks,
            "server-talks" => Self::ServerTalks,
            other => panic!("unexpected protocol {other}"),
        }
    }

    pub async fn run<IO>(self, mut io: IO, is_server: bool)
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        match (self, is_server) {
            (Self::Echo, true) => {
                let mut buf = [0_u8; 6];

                for _ in 0..10 {
                    let read_size = io.read(&mut buf).await.unwrap();
                    if read_size == 0 {
                        break;
                    }

                    io.write_all(buf.get(..read_size).unwrap()).await.unwrap();
                    io.flush().await.unwrap();
                }

                io.shutdown().await.unwrap();
            }

            (Self::Echo, false) => {
                let mut buf = [0_u8; 6];

                for _ in 0..10 {
                    io.write_all(b"hello\n").await.unwrap();
                    let read_size = io.read_exact(&mut buf).await.unwrap();
                    assert_eq!(read_size, 6);
                    assert_eq!(&buf, b"hello\n");
                }

                io.shutdown().await.unwrap();

                let read_size = io.read(&mut buf).await.unwrap();
                assert_eq!(read_size, 0);
            }

            (Self::ClientTalks, false) | (Self::ServerTalks, true) => {
                let mut buf = [0_u8; 1];
                assert_eq!(io.read(&mut buf).await.unwrap(), 0,);

                for _ in 0..10 {
                    io.write_all(b"hello\n").await.unwrap();
                    io.flush().await.unwrap();
                }

                io.shutdown().await.unwrap();
            }

            (Self::ClientTalks, true) | (Self::ServerTalks, false) => {
                io.shutdown().await.unwrap();

                let mut buf = [0_u8; 6];

                for _ in 0..10 {
                    let read_size = io.read_exact(&mut buf).await.unwrap();
                    assert_eq!(read_size, 6);
                    assert_eq!(&buf, b"hello\n");
                }

                let read_size = io.read(&mut buf).await.unwrap();
                assert_eq!(read_size, 0);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum HttpKind {
    Http1,
    Http2,
    Http1NoAlpn,
    Http2NoAlpn,
    Http1Alpn,
    Http2Alpn,
}

impl HttpKind {
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

pub async fn test_http_server(
    stream: TcpStream,
    version: HttpVersion,
    acceptor: Option<&TlsAcceptor>,
) {
    let mut request_tasks = JoinSet::new();

    let stream = match acceptor {
        Some(acceptor) => {
            let stream = acceptor.accept(stream).await.unwrap();
            MaybeTls::Tls(Box::new(TlsStream::Server(stream)))
        }
        None => MaybeTls::NoTls(stream),
    };
    let mut requests = ExtractedRequests::new(TokioIo::new(stream), version);

    while let Some(request) = requests.next().await {
        let request = request.unwrap();
        println!("[{}:{}] Got request: {request:?}", file!(), line!());

        request_tasks.spawn(async move {
            let ExtractedRequest {
                parts,
                body_head,
                body_tail,
                upgrade,
                response_tx,
            } = request;

            match TcpProtocolKind::from_request(&parts) {
                Some(kind) => {
                    assert!(body_head.is_empty());
                    assert!(body_tail.is_none());
                    let response = Response::builder()
                        .status(StatusCode::SWITCHING_PROTOCOLS)
                        .header(header::CONNECTION, "upgrade")
                        .header(header::UPGRADE, kind.name())
                        .body(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed())
                        .unwrap();
                    response_tx.send(response).unwrap();
                    let io = upgrade.into_inner().await.unwrap();
                    kind.run(TokioIo::new(io), true).await;
                }
                None => {
                    let body = RolledBackBody {
                        head: body_head.into_iter(),
                        tail: body_tail,
                    };
                    let response = Response::builder()
                        .status(StatusCode::IM_A_TEAPOT)
                        .body(body.into())
                        .unwrap();
                    response_tx.send(response).unwrap();
                }
            }
        });
    }

    request_tasks.join_all().await;
}

pub async fn send_echo_request(
    sender: &mut HttpSender<BoxBody<Bytes, hyper::Error>>,
    https: bool,
    path: &str,
    user_header: Option<i32>,
) {
    println!(
        "[{}:{}] Sending echo request, https={https}, path={path}, user_header={user_header:?}",
        file!(),
        line!()
    );

    let (frame_tx, frame_rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
    tokio::spawn(async move {
        for _ in 0..10 {
            let frame = Frame::data(Bytes::from(b"hello\n".as_slice()));
            frame_tx.send(Ok(frame)).await.unwrap();
        }
    });

    let mut builder = Request::builder().method(Method::POST).uri(format!(
        "{}://server{}",
        if https { "https" } else { "http" },
        path
    ));
    if let Some(user) = user_header {
        builder = builder.header("x-user", user.to_string());
    }
    let request = builder
        .body(BoxBody::new(StreamBody::new(ReceiverStream::new(frame_rx))))
        .unwrap();

    let response = sender.send(request).await.unwrap();
    println!("[{}:{}] Got response: {response:?}", file!(), line!());

    assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        body.as_ref(),
        b"hello\nhello\nhello\nhello\nhello\nhello\nhello\nhello\nhello\nhello\n"
    );
    println!("[{}:{}] Request finished", file!(), line!());
}

pub async fn send_upgrade_request(
    sender: &mut HttpSender<BoxBody<Bytes, hyper::Error>>,
    https: bool,
    path: &str,
    user_header: Option<i32>,
    protocol: TcpProtocolKind,
) {
    println!("[{}:{}] Sending upgrade request, https={https}, path={path}, user_header={user_header:?}, protocol={protocol:?}", file!(), line!());

    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "{}://server{}",
            if https { "https" } else { "http" },
            path
        ))
        .header(header::CONNECTION, HeaderValue::from_static("upgrade"))
        .header(
            header::UPGRADE,
            HeaderValue::from_str(protocol.name()).unwrap(),
        );
    if let Some(user) = user_header {
        builder = builder.header("x-user", user.to_string());
    }
    let request = builder
        .body(BoxBody::new(
            Empty::<Bytes>::new().map_err(|_| unreachable!()),
        ))
        .unwrap();

    let mut response = sender.send(request).await.unwrap();
    println!("[{}:{}] Got response: {response:?}", file!(), line!());

    let upgrade = hyper::upgrade::on(&mut response);
    assert_eq!(
        TcpProtocolKind::from_response(&response.into_parts().0),
        protocol,
    );
    let io = upgrade.await.unwrap();
    println!(
        "[{}:{}] Connection upgraded, running protocol {protocol:?}",
        file!(),
        line!()
    );
    protocol.run(TokioIo::new(io), false).await;
    println!("[{}:{}] Connection closed", file!(), line!());
}
