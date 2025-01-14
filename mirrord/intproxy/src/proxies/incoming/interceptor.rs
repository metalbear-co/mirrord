//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted connection.

use std::{
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use hyper::{body::Frame, upgrade::OnUpgrade, Response, StatusCode};
use hyper_util::rt::TokioIo;
use mirrord_protocol::tcp::{HttpRequest, HttpResponse, InternalHttpResponse};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time,
};
use tracing::Level;

use super::{
    http::{LocalHttpClient, LocalHttpError, PeekedBody},
    streaming_body::StreamingBody,
};
use crate::{
    background_tasks::{BackgroundTask, MessageBus, PeekableMessageBus},
    proxies::incoming::bound_socket::BoundTcpSocket,
};

/// Messages consumed by the [`Interceptor`] when it runs as a [`BackgroundTask`].
pub enum MessageIn {
    /// Request to be sent to the user application.
    Http(HttpRequest<StreamingBody>),
    /// Data to be sent to the user application.
    Raw(Vec<u8>),
}

/// Messages produced by the [`Interceptor`] when it runs as a [`BackgroundTask`].
#[derive(Debug)]
pub enum MessageOut {
    /// Response received from the user application.
    Http(HttpResponse<PeekedBody>),
    /// Data received from the user application.
    Raw(Vec<u8>),
}

impl From<HttpRequest<StreamingBody>> for MessageIn {
    fn from(value: HttpRequest<StreamingBody>) -> Self {
        Self::Http(value)
    }
}

impl From<Vec<u8>> for MessageIn {
    fn from(value: Vec<u8>) -> Self {
        Self::Raw(value)
    }
}

/// Errors that can occur when [`Interceptor`] runs as a [`BackgroundTask`].
///
/// All of these are **fatal** for the interceptor and should terminate its main loop
/// ([`Interceptor::run`]).
///
/// HTTP error handling and retries are done in the [`LocalHttpClient`].
#[derive(Error, Debug)]
pub enum InterceptorError {
    #[error("failed to connect to the user application socket: {0}")]
    ConnectFailed(#[source] io::Error),

    #[error("io on the connection with the user application failed: {0}")]
    IoFailed(#[source] io::Error),

    #[error("received an unexpected raw data ({} bytes)", .0.len())]
    UnexpectedRawData(Vec<u8>),

    #[error("received an unexpected HTTP request: {0:?}")]
    UnexpectedHttpRequest(HttpRequest<StreamingBody>),

    #[error(transparent)]
    HttpFailed(#[from] LocalHttpError),

    #[error("failed to set up a TCP socket: {0}")]
    SocketSetupFailed(#[source] io::Error),

    #[error("failed to handle an HTTP upgrade: {0}")]
    HttpUpgradeFailed(#[source] hyper::Error),
}

pub type InterceptorResult<T, E = InterceptorError> = core::result::Result<T, E>;

/// Manages a single intercepted connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`IncomingProxy`](super::IncomingProxy)
/// to manage individual connections.
///
/// This interceptor can proxy both raw TCP data and HTTP messages in the same TCP connection.
/// When it receives [`MessageIn::Raw`], it starts acting as a simple proxy.
/// When it received [`MessageIn::Http`], it starts acting as an HTTP gateway.
pub struct Interceptor {
    /// Socket that should be used to make the first connection (should already be bound).
    socket: BoundTcpSocket,
    /// Address of user app's listener.
    peer: SocketAddr,
}

impl Interceptor {
    /// Creates a new instance. When run, this instance will use the given `socket` (must be already
    /// bound) to communicate with the given `peer`.
    ///
    /// # Note
    ///
    /// The socket can be replaced when retrying HTTP requests.
    pub fn new(socket: BoundTcpSocket, peer: SocketAddr) -> Self {
        Self { socket, peer }
    }
}

impl BackgroundTask for Interceptor {
    type Error = InterceptorError;
    type MessageIn = MessageIn;
    type MessageOut = MessageOut;

    #[tracing::instrument(level = Level::TRACE, skip_all, err)]
    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let stream = self
            .socket
            .connect(self.peer)
            .await
            .map_err(InterceptorError::ConnectFailed)?;
        let mut message_bus = message_bus.peekable();

        tokio::select! {
            // If there is some data from the user application before we get anything from the agent,
            // then this is not HTTP.
            //
            // We should not block until the agent has something, we don't know what this protocol looks like.
            result = stream.readable() => {
                result.map_err(InterceptorError::IoFailed)?;
                return RawConnection { stream }.run(message_bus).await;
            }

            message = message_bus.peek() => match message {
                Some(MessageIn::Http(..)) => {}

                Some(MessageIn::Raw(..)) => {
                    return RawConnection { stream }.run(message_bus).await;
                }

                None => return Ok(()),
            }
        }

        let http_conn = HttpConnection {
            local_client: LocalHttpClient::new_for_stream(stream)?,
        };

        http_conn.run(message_bus).await
    }
}

/// Utilized by the [`Interceptor`] when it acts as an HTTP gateway.
/// See [`HttpConnection::run`] for usage.
struct HttpConnection {
    local_client: LocalHttpClient,
}

impl HttpConnection {
    /// Handles the result of sending an HTTP request.
    /// Returns an [`HttpResponse`] to be returned to the client or an [`InterceptorError`] when the
    /// given [`LocalHttpError`] is fatal for the interceptor. Most [`LocalHttpError`]s are not
    /// fatal and should be converted to [`StatusCode::BAD_GATEWAY`] responses instead.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::WARN))]
    fn handle_send_result(
        request: HttpRequest<StreamingBody>,
        response: Result<Response<PeekedBody>, LocalHttpError>,
    ) -> InterceptorResult<(HttpResponse<PeekedBody>, Option<OnUpgrade>)> {
        match response {
            Err(LocalHttpError::SocketSetupFailed(error)) => {
                Err(InterceptorError::SocketSetupFailed(error))
            }

            Err(LocalHttpError::UnsupportedHttpVersion(..)) => {
                Err(InterceptorError::UnexpectedHttpRequest(request))
            }

            Err(error) => {
                let response = error
                    .as_error_response(
                        request.internal_request.version,
                        request.request_id,
                        request.connection_id,
                        request.port,
                    )
                    .map_body(|body| PeekedBody {
                        head: vec![Frame::data(Bytes::from_owner(body))],
                        tail: None,
                    });

                Ok((response, None))
            }

            Ok(mut response) => {
                let upgrade = if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                    Some(hyper::upgrade::on(&mut response))
                } else {
                    None
                };

                let (parts, body) = response.into_parts();
                let response = HttpResponse {
                    port: request.port,
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    internal_response: InternalHttpResponse {
                        status: parts.status,
                        version: parts.version,
                        headers: parts.headers,
                        body,
                    },
                };

                Ok((response, upgrade))
            }
        }
    }

    /// Proxies HTTP messages until an HTTP upgrade happens or the [`MessageBus`] closes.
    /// Support retries (with reconnecting to the HTTP server).
    ///
    /// When an HTTP upgrade happens, the underlying [`TcpStream`] is reclaimed and wrapped
    /// in a [`RawConnection`], which handles the rest of the connection.
    #[tracing::instrument(level = Level::TRACE, skip_all, ret, err)]
    async fn run(
        mut self,
        mut message_bus: PeekableMessageBus<'_, Interceptor>,
    ) -> InterceptorResult<()> {
        let upgrade = loop {
            match message_bus.recv().await {
                None => return Ok(()),

                Some(MessageIn::Raw(data)) => {
                    // We should not receive any raw data from the agent before sending a
                    //`101 SWITCHING PROTOCOLS` response.
                    return Err(InterceptorError::UnexpectedRawData(data));
                }

                Some(MessageIn::Http(request)) => {
                    let result = self.local_client.send_request(&request).await;
                    let (res, on_upgrade) = Self::handle_send_result(request, result)?;
                    message_bus.send(MessageOut::Http(res)).await;

                    if let Some(on_upgrade) = on_upgrade {
                        break on_upgrade
                            .await
                            .map_err(InterceptorError::HttpUpgradeFailed)?;
                    }
                }
            }
        };

        let parts = upgrade
            .downcast::<TokioIo<TcpStream>>()
            .expect("IO type is known");
        let stream = parts.io.into_inner();
        let read_buf = parts.read_buf;

        if !read_buf.is_empty() {
            message_bus.send(MessageOut::Raw(read_buf.into())).await;
        }

        RawConnection { stream }.run(message_bus).await
    }
}

/// Utilized by the [`Interceptor`] when it acts as a TCP proxy.
/// See [`RawConnection::run`] for usage.
#[derive(Debug)]
struct RawConnection {
    /// Connection between the [`Interceptor`] and the server.
    stream: TcpStream,
}

impl RawConnection {
    /// Proxies raw TCP data until the [`MessageBus`] closes.
    ///
    /// # Notes
    ///
    /// 1. When the peer shuts down writing, a single 0-sized read is sent through the
    ///    [`MessageBus`]. This is to notify the agent about the shutdown condition.
    ///
    /// 2. A 0-sized read received from the [`MessageBus`] is treated as a shutdown on the agent
    ///    side. Connection with the peer is shut down as well.
    ///
    /// 3. This implementation exits only when an error is encountered or the [`MessageBus`] is
    ///    closed.
    async fn run<'a>(
        mut self,
        mut message_bus: PeekableMessageBus<'a, Interceptor>,
    ) -> InterceptorResult<()> {
        let mut buf = BytesMut::with_capacity(64 * 1024);
        let mut reading_closed = false;
        let mut remote_closed = false;

        loop {
            tokio::select! {
                biased;

                res = self.stream.read_buf(&mut buf), if !reading_closed => match res {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => break Err(InterceptorError::IoFailed(e)),
                    Ok(..) => {
                        if buf.is_empty() {
                            tracing::trace!("incoming interceptor -> layer shutdown, sending a 0-sized read to inform the agent");
                            reading_closed = true;
                        }
                        message_bus.send(MessageOut::Raw(buf.to_vec())).await;
                        buf.clear();
                    }
                },

                msg = message_bus.recv(), if !remote_closed => match msg {
                    None => {
                        tracing::trace!("incoming interceptor -> message bus closed, waiting 1 second before exiting");
                        remote_closed = true;
                    },
                    Some(MessageIn::Raw(data)) => {
                        if data.is_empty() {
                            tracing::trace!("incoming interceptor -> agent shutdown, shutting down connection with layer");
                            self.stream.shutdown().await.map_err(InterceptorError::IoFailed)?;
                        } else {
                            self.stream.write_all(&data).await.map_err(InterceptorError::IoFailed)?;
                        }
                    },
                    Some(MessageIn::Http(request)) => break Err(InterceptorError::UnexpectedHttpRequest(request)),
                },

                _ = time::sleep(Duration::from_secs(1)), if remote_closed => {
                    tracing::trace!("incoming interceptor -> layer silent for 1 second and message bus is closed, exiting");

                    break Ok(());
                },
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{convert::Infallible, net::Ipv4Addr, sync::Arc};

    use bytes::Bytes;
    use futures::future::FutureExt;
    use http_body_util::{BodyExt, Empty};
    use hyper::{
        body::Incoming,
        header::{HeaderValue, CONNECTION, UPGRADE},
        server::conn::http1,
        service::service_fn,
        upgrade::Upgraded,
        Method, Request, Response, Version,
    };
    use hyper_util::rt::TokioIo;
    use mirrord_protocol::tcp::{HttpRequest, InternalHttpBodyFrame, InternalHttpRequest};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{watch, Notify},
        task,
    };

    use super::*;
    use crate::background_tasks::{BackgroundTasks, TaskUpdate};

    /// Binary protocol over TCP.
    /// Server first sends bytes [`INITIAL_MESSAGE`], then echoes back all received data.
    const TEST_PROTO: &str = "dummyecho";

    const INITIAL_MESSAGE: &[u8] = &[0x4a, 0x50, 0x32, 0x47, 0x4d, 0x44];

    /// Handles requests upgrading to the [`TEST_PROTO`] protocol.
    async fn upgrade_req_handler(
        mut req: Request<Incoming>,
    ) -> hyper::Result<Response<Empty<Bytes>>> {
        async fn dummy_echo(upgraded: Upgraded) -> io::Result<()> {
            let mut upgraded = TokioIo::new(upgraded);
            let mut buf = [0_u8; 64];

            upgraded.write_all(INITIAL_MESSAGE).await?;

            loop {
                let bytes_read = upgraded.read(&mut buf[..]).await?;
                if bytes_read == 0 {
                    break;
                }

                let echo_back = buf.get(0..bytes_read).unwrap();
                upgraded.write_all(echo_back).await?;
            }

            Ok(())
        }

        let mut res = Response::new(Empty::new());

        let contains_expected_upgrade = req
            .headers()
            .get(UPGRADE)
            .filter(|proto| *proto == TEST_PROTO)
            .is_some();
        if !contains_expected_upgrade {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }

        task::spawn(async move {
            match hyper::upgrade::on(&mut req).await {
                Ok(upgraded) => {
                    if let Err(e) = dummy_echo(upgraded).await {
                        eprintln!("server foobar io error: {}", e)
                    };
                }
                Err(e) => eprintln!("upgrade error: {}", e),
            }
        });

        *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        res.headers_mut()
            .insert(UPGRADE, HeaderValue::from_static(TEST_PROTO));
        res.headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("upgrade"));
        Ok(res)
    }

    /// Runs a [`hyper`] server that accepts only requests upgrading to the [`TEST_PROTO`] protocol.
    async fn dummy_echo_server(listener: TcpListener, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                res = listener.accept() => {
                    let (stream, _) = res.expect("dummy echo server failed to accept connection");

                    let mut shutdown = shutdown.clone();

                    task::spawn(async move {
                        let conn = http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(upgrade_req_handler));
                        let mut conn = conn.with_upgrades();
                        let mut conn = Pin::new(&mut conn);

                        tokio::select! {
                            res = &mut conn => {
                                res.expect("dummy echo server failed to serve connection");
                            }

                            _ = shutdown.changed() => {
                                conn.graceful_shutdown();
                            }
                        }
                    });
                }

                _ = shutdown.changed() => break,
            }
        }
    }

    #[tokio::test]
    async fn upgrade_test() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server_task = task::spawn(dummy_echo_server(listener, shutdown_rx));

        let mut tasks: BackgroundTasks<(), MessageOut, InterceptorError> = Default::default();
        let interceptor = {
            let socket =
                BoundTcpSocket::bind_specified_or_localhost(Ipv4Addr::LOCALHOST.into()).unwrap();
            tasks.register(Interceptor::new(socket, local_destination), (), 8)
        };

        interceptor
            .send(HttpRequest {
                connection_id: 0,
                request_id: 0,
                port: 80,
                internal_request: InternalHttpRequest {
                    method: Method::GET,
                    uri: "dummyecho://www.mirrord.dev/".parse().unwrap(),
                    headers: [
                        (CONNECTION, HeaderValue::from_static("upgrade")),
                        (UPGRADE, HeaderValue::from_static(TEST_PROTO)),
                    ]
                    .into_iter()
                    .collect(),
                    version: Version::HTTP_11,
                    body: Default::default(),
                },
            })
            .await;

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(MessageOut::Http(res)) => {
                assert_eq!(
                    res.internal_response.status,
                    StatusCode::SWITCHING_PROTOCOLS
                );
                println!("Received repsonse from the interceptor: {res:?}");
                assert!(res
                    .internal_response
                    .headers
                    .get(CONNECTION)
                    .filter(|v| *v == "upgrade")
                    .is_some());
                assert!(res
                    .internal_response
                    .headers
                    .get(UPGRADE)
                    .filter(|v| *v == TEST_PROTO)
                    .is_some());
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        interceptor.send(b"test test test".to_vec()).await;

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(MessageOut::Raw(bytes)) => {
                assert_eq!(bytes, INITIAL_MESSAGE);
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(MessageOut::Raw(bytes)) => {
                assert_eq!(bytes, b"test test test");
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let _ = shutdown_tx.send(true);
        server_task.await.expect("dummy echo server panicked");
    }

    /// Ensure that body of [`MessageOut::Http`] response is received frame by frame
    #[tokio::test]
    async fn receive_request_as_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        let mut tasks: BackgroundTasks<(), MessageOut, InterceptorError> = Default::default();
        let interceptor = Interceptor::new(
            BoundTcpSocket::bind_specified_or_localhost(Ipv4Addr::LOCALHOST.into()).unwrap(),
            listener.local_addr().unwrap(),
        );
        let interceptor = tasks.register(interceptor, (), 8);

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(1);
        interceptor
            .send(MessageIn::Http(HttpRequest {
                internal_request: InternalHttpRequest {
                    method: Method::POST,
                    uri: "/".parse().unwrap(),
                    headers: Default::default(),
                    version: Version::HTTP_11,
                    body: StreamingBody::from(frame_rx),
                },
                connection_id: 1,
                request_id: 2,
                port: 3,
            }))
            .await;
        let connection = listener.accept().await.unwrap().0;

        let notifier = Arc::new(Notify::default());

        // Task that sends the next frame when notified.
        // Sends two frames, then exits.
        tokio::spawn({
            let notifier = notifier.clone();
            async move {
                for _ in 0..2 {
                    notifier.notified().await;
                    if frame_tx
                        .send(InternalHttpBodyFrame::Data(b"some-data".into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }

                // Wait for third notification before dropping the frame sender.
                notifier.notified().await;
            }
        });

        let service = service_fn(|mut req: Request<Incoming>| {
            let notifier = notifier.clone();
            async move {
                for _ in 0..2 {
                    let frame = req.body_mut().frame().now_or_never();
                    assert!(frame.is_none());

                    notifier.notify_one();
                    let frame = req
                        .body_mut()
                        .frame()
                        .await
                        .unwrap()
                        .unwrap()
                        .into_data()
                        .unwrap();
                    assert_eq!(frame, b"some-data".to_vec());
                    let frame = req.body_mut().frame().now_or_never();
                    assert!(frame.is_none());
                }

                notifier.notify_one();
                let frame = req.body_mut().frame().await;
                assert!(frame.is_none());

                Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new()))
            }
        });
        let conn = http1::Builder::new().serve_connection(TokioIo::new(connection), service);
        conn.await.unwrap();
    }
}
