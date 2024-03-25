//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted connection.

use std::{
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};

use bytes::BytesMut;
use hyper::{upgrade::OnUpgrade, StatusCode, Version};
use hyper_util::rt::TokioIo;
use mirrord_protocol::tcp::{
    HttpRequestFallback, HttpResponse, HttpResponseFallback, InternalHttpBody,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpSocket, TcpStream},
    time,
};

use super::http::HttpSender;
use crate::background_tasks::{BackgroundTask, MessageBus};

/// Messages consumed by the [`Interceptor`] when it runs as a [`BackgroundTask`].
pub enum MessageIn {
    /// Request to be sent to the user application.
    Http(HttpRequestFallback),
    /// Data to be sent to the user application.
    Raw(Vec<u8>),
}

/// Messages produced by the [`Interceptor`] when it runs as a [`BackgroundTask`].
#[derive(Debug)]
pub enum MessageOut {
    /// Response received from the user application.
    Http(HttpResponseFallback),
    /// Data received from the user application.
    Raw(Vec<u8>),
}

impl From<HttpRequestFallback> for MessageIn {
    fn from(value: HttpRequestFallback) -> Self {
        Self::Http(value)
    }
}

impl From<Vec<u8>> for MessageIn {
    fn from(value: Vec<u8>) -> Self {
        Self::Raw(value)
    }
}

/// Errors that can occur when [`Interceptor`] runs as a [`BackgroundTask`].
#[derive(Error, Debug)]
pub enum InterceptorError {
    /// IO failed.
    #[error("io failed: {0}")]
    Io(#[from] io::Error),
    /// Hyper failed.
    #[error("hyper failed: {0}")]
    Hyper(#[from] hyper::Error),
    /// The layer closed connection too soon to send a request.
    #[error("connection closed too soon")]
    ConnectionClosedTooSoon(HttpRequestFallback),
    /// Received a request with an unsupported HTTP version.
    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),
    /// Occurs when [`Interceptor`] receives [`MessageIn::Raw`], but it acts as an HTTP gateway and
    /// there was no HTTP upgrade.
    #[error("received raw bytes, but expected an HTTP request")]
    UnexpectedRawData,
    /// Occurs when [`Interceptor`] receives [`MessageIn::Http`], but it acts as a TCP proxy.
    #[error("received an HTTP request, but expected raw bytes")]
    UnexpectedHttpRequest,
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
    socket: TcpSocket,
    peer: SocketAddr,
}

impl Interceptor {
    /// Creates a new instance. When run, this instance will use the given `socket` (must be already
    /// bound) to communicate with the given `peer`.
    ///
    /// # Note
    ///
    /// The socket can be replaced when retrying HTTP requests.
    pub fn new(socket: TcpSocket, peer: SocketAddr) -> Self {
        Self { socket, peer }
    }
}

impl BackgroundTask for Interceptor {
    type Error = InterceptorError;
    type MessageIn = MessageIn;
    type MessageOut = MessageOut;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> InterceptorResult<(), Self::Error> {
        let mut stream = self.socket.connect(self.peer).await?;

        // First, we determine whether this is a raw TCP connection or an HTTP connection.
        // If we receive an HTTP request from our parent task, this must be an HTTP connection.
        // If we receive raw bytes or our peer starts sending some data, this must be raw TCP.
        let request = tokio::select! {
            message = message_bus.recv() => match message {
                Some(MessageIn::Raw(data)) => {
                    stream.write_all(&data).await?;
                    return RawConnection { stream }.run(message_bus).await;
                }
                Some(MessageIn::Http(request)) => request,
                None => return Ok(()),
            },

            result = stream.readable() => {
                result?;
                return RawConnection { stream }.run(message_bus).await;
            }
        };

        let sender = super::http::handshake(request.version(), stream).await?;
        let mut http_conn = HttpConnection {
            sender,
            peer: self.peer,
        };
        let (response, on_upgrade) = http_conn.send(request).await?;
        message_bus.send(MessageOut::Http(response)).await;

        let raw = if let Some(on_upgrade) = on_upgrade {
            let upgraded = on_upgrade.await?;
            let parts = upgraded
                .downcast::<TokioIo<TcpStream>>()
                .expect("IO type is known");
            if !parts.read_buf.is_empty() {
                message_bus
                    .send(MessageOut::Raw(parts.read_buf.into()))
                    .await;
            }

            Some(RawConnection {
                stream: parts.io.into_inner(),
            })
        } else {
            http_conn.run(message_bus).await?
        };

        if let Some(raw) = raw {
            raw.run(message_bus).await
        } else {
            Ok(())
        }
    }
}

/// Utilized by the [`Interceptor`] when it acts as an HTTP gateway.
/// See [`HttpConnection::run`] for usage.
struct HttpConnection {
    /// Server address saved to allow for reconnecting in case a retry is required.
    peer: SocketAddr,
    /// Handle to the HTTP connection between the [`Interceptor`] the server.
    sender: HttpSender,
}

impl HttpConnection {
    /// Handles the result of sending an HTTP request.
    /// Returns an [`HttpResponseFallback`] to be returned to the client or an [`InterceptorError`].
    async fn handle_response(
        &self,
        request: HttpRequestFallback,
        response: InterceptorResult<hyper::Response<hyper::body::Incoming>>,
    ) -> InterceptorResult<(HttpResponseFallback, Option<OnUpgrade>)> {
        match response {
            Err(InterceptorError::Hyper(e)) if e.is_closed() => {
                tracing::warn!(
                    "Sending request to local application failed with: {e:?}. \
                        Seems like the local application closed the connection too early, so \
                        creating a new connection and trying again."
                );
                tracing::trace!("The request to be retried: {request:?}.");

                Err(InterceptorError::ConnectionClosedTooSoon(request))
            }

            Err(InterceptorError::Hyper(e)) if e.is_parse() => {
                tracing::warn!(
                    "Could not parse HTTP response to filtered HTTP request, got error: {e:?}."
                );
                let body_message = format!(
                    "mirrord: could not parse HTTP response from local application - {e:?}"
                );
                Ok((
                    HttpResponseFallback::response_from_request(
                        request,
                        StatusCode::BAD_GATEWAY,
                        &body_message,
                    ),
                    None,
                ))
            }

            Err(err) => {
                tracing::warn!("Request to local application failed with: {err:?}.");
                let body_message = format!(
                    "mirrord tried to forward the request to the local application and got {err:?}"
                );
                Ok((
                    HttpResponseFallback::response_from_request(
                        request,
                        StatusCode::BAD_GATEWAY,
                        &body_message,
                    ),
                    None,
                ))
            }

            Ok(mut res) => {
                let upgrade = if res.status() == StatusCode::SWITCHING_PROTOCOLS {
                    Some(hyper::upgrade::on(&mut res))
                } else {
                    None
                };

                let result = match &request {
                    HttpRequestFallback::Framed(..) => {
                        HttpResponse::<InternalHttpBody>::from_hyper_response(
                            res,
                            self.peer.port(),
                            request.connection_id(),
                            request.request_id(),
                        )
                        .await
                        .map(HttpResponseFallback::Framed)
                    }
                    HttpRequestFallback::Fallback(..) => {
                        HttpResponse::<Vec<u8>>::from_hyper_response(
                            res,
                            self.peer.port(),
                            request.connection_id(),
                            request.request_id(),
                        )
                        .await
                        .map(HttpResponseFallback::Fallback)
                    }
                };

                Ok(result
                    .map(|response| (response, upgrade))
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "Failed to read response to filtered http request: {e:?}. \
                            Please consider reporting this issue on \
                            https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml"
                        );

                        (
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            ),
                            None,
                        )
                    })
                )
            }
        }
    }

    /// Sends the given [`HttpRequestFallback`] to the server.
    /// If the HTTP connection with server is closed too soon, starts a new connection and retries
    /// once. Returns [`HttpResponseFallback`] from the server.
    async fn send(
        &mut self,
        request: HttpRequestFallback,
    ) -> InterceptorResult<(HttpResponseFallback, Option<OnUpgrade>)> {
        let response = self.sender.send(request.clone()).await;
        let response = self.handle_response(request, response).await;

        let Err(InterceptorError::ConnectionClosedTooSoon(request)) = response else {
            return response;
        };

        tracing::trace!("Request {request:?} connection was closed too soon, retrying once");

        // Create a new connection for this second attempt.
        let socket = super::bind_similar(self.peer)?;
        let stream = socket.connect(self.peer).await?;
        let new_sender = super::http::handshake(request.version(), stream).await?;
        self.sender = new_sender;

        let response = self.sender.send(request.clone()).await;
        self.handle_response(request, response).await
    }

    /// Proxies HTTP messages until an HTTP upgrade happens or the [`MessageBus`] closes.
    /// Support retries (with reconnecting to the HTTP server).
    ///
    /// When an HTTP upgrade happens, the underlying [`TcpStream`] is reclaimed, wrapped
    /// in a [`RawConnection`] and returned. When [`MessageBus`] closes, [`None`] is returned.
    async fn run(
        mut self,
        message_bus: &mut MessageBus<Interceptor>,
    ) -> InterceptorResult<Option<RawConnection>> {
        let upgrade = loop {
            let Some(msg) = message_bus.recv().await else {
                return Ok(None);
            };

            match msg {
                MessageIn::Raw(..) => {
                    // We should not receive any raw data from the agent before sending a
                    //`101 SWITCHING PROTOCOLS` response.
                    return Err(InterceptorError::UnexpectedRawData);
                }

                MessageIn::Http(req) => {
                    let (res, on_upgrade) = self.send(req).await?;
                    println!("{} has upgrade: {}", res.request_id(), on_upgrade.is_some());
                    message_bus.send(MessageOut::Http(res)).await;

                    if let Some(on_upgrade) = on_upgrade {
                        break on_upgrade.await?;
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

        Ok(Some(RawConnection { stream }))
    }
}

/// Utilized by the [`Interceptor`] when it acts as a TCP proxy.
/// See [`RawConnection::run`] for usage.
struct RawConnection {
    /// Connection between the [`Interceptor`] and the server.
    stream: TcpStream,
}

impl RawConnection {
    /// Proxies raw TCP data until the [`MessageBus`] closes.
    ///
    /// # Note
    ///
    /// When the peer shuts down writing, a single 0-sized read is sent through
    /// the [`MessageBus`]. This is to notify the agent about the shutdown condition.
    async fn run(mut self, message_bus: &mut MessageBus<Interceptor>) -> InterceptorResult<()> {
        let mut buf = BytesMut::with_capacity(64 * 1024);
        let mut remote_closed = false;
        let mut reading_closed = false;

        loop {
            tokio::select! {
                biased;

                res = self.stream.read_buf(&mut buf), if !reading_closed => match res {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => break Err(e.into()),
                    Ok(0) => {
                        tracing::trace!("layer shut down writing, sending a 0-sized read to inform the agent");
                        message_bus.send(MessageOut::Raw(vec![])).await;
                        reading_closed = true;
                    }
                    Ok(..) => {
                        message_bus.send(MessageOut::Raw(buf.to_vec())).await;
                        buf.clear();
                    }
                },

                msg = message_bus.recv(), if !remote_closed => match msg {
                    None => {
                        tracing::trace!("message bus closed, waiting 1 second before exiting");
                        remote_closed = true;
                    },
                    Some(MessageIn::Raw(data)) => {
                        self.stream.write_all(&data).await?;
                    }
                    Some(MessageIn::Http(..)) => break Err(InterceptorError::UnexpectedHttpRequest),
                },

                _ = time::sleep(Duration::from_secs(1)), if remote_closed => {
                    tracing::trace!("layer silent for 1 second and message bus is closed, exiting");

                    break Ok(());
                },
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http_body_util::Empty;
    use hyper::{
        body::Incoming,
        header::{HeaderValue, CONNECTION, UPGRADE},
        server::conn::http1,
        service::service_fn,
        upgrade::Upgraded,
        Method, Request, Response,
    };
    use hyper_util::rt::TokioIo;
    use mirrord_protocol::tcp::{HttpRequest, InternalHttpRequest};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::watch,
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

                upgraded.write_all(&buf[..bytes_read]).await?;
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
            let socket = TcpSocket::new_v4().unwrap();
            socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
            tasks.register(Interceptor::new(socket, local_destination), (), 8)
        };

        interceptor
            .send(HttpRequestFallback::Fallback(HttpRequest {
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
            }))
            .await;

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(MessageOut::Http(res)) => {
                let res = res
                    .into_hyper::<Infallible>()
                    .expect("failed to convert into hyper response");
                assert_eq!(res.status(), StatusCode::SWITCHING_PROTOCOLS);
                println!("{:?}", res.headers());
                assert!(res
                    .headers()
                    .get(CONNECTION)
                    .filter(|v| *v == "upgrade")
                    .is_some());
                assert!(res
                    .headers()
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
}
