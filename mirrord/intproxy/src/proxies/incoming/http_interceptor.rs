//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted HTTP connection.

use std::{
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};

use hyper::{StatusCode, Version};
use mirrord_protocol::tcp::{
    HttpRequestFallback, HttpResponse, HttpResponseFallback, InternalHttpBody,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpSocket, TcpStream},
    sync::oneshot::Receiver,
    time,
};

use super::{
    http::{HttpSender, UpgradedConnection},
    InterceptorMessageOut,
};
use crate::background_tasks::{BackgroundTask, MessageBus};

pub enum HttpInterceptorMessageIn {
    HttpRequest(HttpRequestFallback),
    RawData(Vec<u8>),
}

impl From<HttpRequestFallback> for HttpInterceptorMessageIn {
    fn from(value: HttpRequestFallback) -> Self {
        Self::HttpRequest(value)
    }
}

impl From<Vec<u8>> for HttpInterceptorMessageIn {
    fn from(value: Vec<u8>) -> Self {
        Self::RawData(value)
    }
}

/// Errors that can occur when executing [`HttpInterceptor`] as a [`BackgroundTask`].
#[derive(Error, Debug)]
pub enum HttpInterceptorError {
    /// IO failed.
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    /// Hyper failed.
    #[error("hyper failed: {0}")]
    HyperError(#[from] hyper::Error),
    /// The layer closed connection too soon to send a request.
    #[error("connection closed too soon")]
    ConnectionClosedTooSoon(HttpRequestFallback),
    /// Received a request with unsupported HTTP version.
    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),
    #[error("upgrading connections not supported in {0:?}")]
    UpgradeNotSupported(Version),
    #[error("task for keeping http connection alive panicked")]
    HttpConnectionTaskPanicked,
}

pub type Result<T, E = HttpInterceptorError> = core::result::Result<T, E>;

/// Manages a single intercepted HTTP connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`IncomingProxy`](super::IncomingProxy)
/// to manage individual connections.
pub struct HttpInterceptor {
    socket: TcpSocket,
    local_destination: SocketAddr,
}

impl HttpInterceptor {
    /// Crates a new instance. This instance will send requests to the given `local_destination`.
    pub fn new(socket: TcpSocket, local_destination: SocketAddr) -> Self {
        Self {
            socket,
            local_destination,
        }
    }
}

impl BackgroundTask for HttpInterceptor {
    type Error = HttpInterceptorError;
    type MessageIn = HttpInterceptorMessageIn;
    type MessageOut = InterceptorMessageOut;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let stream = self.socket.connect(self.local_destination).await?;
        let mut conn = GenericConnection::Raw(RawConnection { stream });

        loop {
            let new_conn = match conn {
                GenericConnection::Raw(raw_conn) => raw_conn
                    .run(message_bus)
                    .await?
                    .map(GenericConnection::Http),
                GenericConnection::Http(http_conn) => http_conn
                    .run(message_bus)
                    .await?
                    .map(GenericConnection::Raw),
            };

            match new_conn {
                Some(new_conn) => conn = new_conn,
                None => break Ok(()),
            }
        }
    }
}

enum GenericConnection {
    Raw(RawConnection),
    Http(HttpConnection),
}

struct HttpConnection {
    peer: SocketAddr,
    sender: HttpSender,
    upgrade_rx: Receiver<Result<UpgradedConnection>>,
}

impl HttpConnection {
    /// Handles the result of sending an HTTP request.
    /// Returns a new request to be sent or an error.
    async fn handle_response(
        &self,
        request: HttpRequestFallback,
        response: Result<hyper::Response<hyper::body::Incoming>, HttpInterceptorError>,
    ) -> Result<HttpResponseFallback, HttpInterceptorError> {
        match response {
                Err(HttpInterceptorError::HyperError(e)) if e.is_closed() => {
                    tracing::warn!(
                        "Sending request to local application failed with: {e:?}. \
                        Seems like the local application closed the connection too early, so \
                        creating a new connection and trying again."
                    );
                    tracing::trace!("The request to be retried: {request:?}.");

                    Err(HttpInterceptorError::ConnectionClosedTooSoon(request))
                }

                Err(HttpInterceptorError::HyperError(e)) if e.is_parse() => {
                    tracing::warn!("Could not parse HTTP response to filtered HTTP request, got error: {e:?}.");
                    let body_message = format!("mirrord: could not parse HTTP response from local application - {e:?}");
                    Ok(HttpResponseFallback::response_from_request(
                        request,
                        StatusCode::BAD_GATEWAY,
                        &body_message,
                    ))
                }

                Err(err) => {
                    tracing::warn!("Request to local application failed with: {err:?}.");
                    let body_message = format!("mirrord tried to forward the request to the local application and got {err:?}");
                    Ok(HttpResponseFallback::response_from_request(
                        request,
                        StatusCode::BAD_GATEWAY,
                        &body_message,
                    ))
                }

                Ok(res) if matches!(request, HttpRequestFallback::Framed(_)) => Ok(
                    HttpResponse::<InternalHttpBody>::from_hyper_response(
                        res,
                        self.peer.port(),
                        request.connection_id(),
                        request.request_id()
                    )
                        .await
                        .map(HttpResponseFallback::Framed)
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "Failed to read response to filtered http request: {e:?}. \
                                Please consider reporting this issue on \
                                https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml"
                            );
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            )
                        }),
                ),

                Ok(res) => Ok(
                    HttpResponse::<Vec<u8>>::from_hyper_response(
                        res, self.peer.port(),
                        request.connection_id(),
                        request.request_id()
                    )
                        .await
                        .map(HttpResponseFallback::Fallback)
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "Failed to read response to filtered http request: {e:?}. \
                                Please consider reporting this issue on \
                                https://github.com/metalbear-co/mirrord/issues/new?labels=bug&template=bug_report.yml"
                            );
                            HttpResponseFallback::response_from_request(
                                request,
                                StatusCode::BAD_GATEWAY,
                                "mirrord",
                            )
                        }),
                ),
            }
    }

    async fn send(&mut self, req: HttpRequestFallback) -> Result<HttpResponseFallback> {
        let res = self.sender.send(req.clone()).await;
        let res = self.handle_response(req, res).await;

        let Err(HttpInterceptorError::ConnectionClosedTooSoon(req)) = res else {
            return res;
        };

        tracing::trace!("Request {req:?} connection was closed too soon, retrying once");

        // Create a new connection for this second attempt.
        let stream = TcpStream::connect(self.peer).await?;
        let (new_sender, new_upgrade_rx) = super::http::handshake(req.version(), stream).await?;
        self.sender = new_sender;
        self.upgrade_rx = new_upgrade_rx;

        let res = self.sender.send(req.clone()).await;
        self.handle_response(req, res).await
    }

    async fn run(
        mut self,
        message_bus: &mut MessageBus<HttpInterceptor>,
    ) -> Result<Option<RawConnection>> {
        while let Some(msg) = message_bus.recv().await {
            match msg {
                HttpInterceptorMessageIn::RawData(data) => {
                    let UpgradedConnection {
                        mut stream,
                        unprocessed_bytes,
                    } = self
                        .upgrade_rx
                        .await
                        .map_err(|_| HttpInterceptorError::HttpConnectionTaskPanicked)??;

                    if !unprocessed_bytes.is_empty() {
                        message_bus
                            .send(InterceptorMessageOut::Bytes(unprocessed_bytes.to_vec()))
                            .await;
                    }

                    stream.write_all(&data).await?;

                    return Ok(Some(RawConnection { stream }));
                }

                HttpInterceptorMessageIn::HttpRequest(req) => {
                    let res = self.send(req).await?;
                    message_bus.send(InterceptorMessageOut::Http(res)).await;
                }
            }
        }

        Ok(None)
    }
}

struct RawConnection {
    stream: TcpStream,
}

impl RawConnection {
    async fn run(
        mut self,
        message_bus: &mut MessageBus<HttpInterceptor>,
    ) -> Result<Option<HttpConnection>> {
        let mut buffer = vec![0; 1024];
        let mut remote_closed = false;
        let mut reading_closed = false;

        loop {
            tokio::select! {
                biased;

                res = self.stream.read(&mut buffer[..]), if !reading_closed => match res {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => break Err(e.into()),
                    Ok(0) => {
                        reading_closed = true;
                    }
                    Ok(read) => {
                        message_bus.send(InterceptorMessageOut::Bytes(buffer.get(..read).unwrap().to_vec())).await;
                    }
                },

                msg = message_bus.recv(), if !remote_closed => match msg {
                    None => {
                        tracing::trace!("message bus closed, waiting 1 second before exiting");
                        remote_closed = true;
                    },
                    Some(HttpInterceptorMessageIn::RawData(data)) => {
                        self.stream.write_all(&data).await?;
                    }
                    Some(HttpInterceptorMessageIn::HttpRequest(req)) => {
                        let mut conn = {
                            let peer = self.stream.peer_addr()?;
                            let (sender, upgrade_rx) = super::http::handshake(req.version(), self.stream).await?;

                            HttpConnection { peer, sender, upgrade_rx }
                        };

                        let res = conn.send(req).await?;

                        message_bus.send(InterceptorMessageOut::Http(res)).await;

                        break Ok(Some(conn))
                    }
                },

                _ = time::sleep(Duration::from_secs(1)), if remote_closed => {
                    tracing::trace!("layer silent for 1 second and message bus is closed, exiting");

                    break Ok(None);
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

    const TEST_PROTO_NAME: &str = "dummyecho";

    async fn upgrade_req_handler(
        mut req: Request<Incoming>,
    ) -> hyper::Result<Response<Empty<Bytes>>> {
        async fn dummy_echo(upgraded: Upgraded) -> io::Result<()> {
            let mut upgraded = TokioIo::new(upgraded);
            let mut buf = [0_u8; 64];

            upgraded.write_all(b"hello").await?;

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
            .filter(|proto| *proto == TEST_PROTO_NAME)
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
            .insert(UPGRADE, HeaderValue::from_static(TEST_PROTO_NAME));
        res.headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("upgrade"));
        Ok(res)
    }

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

        let mut tasks: BackgroundTasks<(), InterceptorMessageOut, HttpInterceptorError> =
            Default::default();
        let interceptor = {
            let socket = TcpSocket::new_v4().unwrap();
            socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
            tasks.register(HttpInterceptor::new(socket, local_destination), (), 8)
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
                        (UPGRADE, HeaderValue::from_static(TEST_PROTO_NAME)),
                    ]
                    .into_iter()
                    .collect(),
                    version: Version::HTTP_11,
                    body: Default::default(),
                },
            }))
            .await;

        interceptor.send(b"test test test".to_vec()).await;

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(InterceptorMessageOut::Http(res)) => {
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
                    .filter(|v| *v == TEST_PROTO_NAME)
                    .is_some());
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(InterceptorMessageOut::Bytes(bytes)) => {
                assert_eq!(bytes, b"hello");
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let (_, update) = tasks.next().await.expect("no task result");
        match update {
            TaskUpdate::Message(InterceptorMessageOut::Bytes(bytes)) => {
                assert_eq!(bytes, b"test test test");
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let _ = shutdown_tx.send(true);
        server_task.await.expect("dummy echo server panicked");
    }
}
