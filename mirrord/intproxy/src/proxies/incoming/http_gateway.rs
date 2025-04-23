use std::{
    collections::VecDeque,
    convert::Infallible,
    error::Report,
    fmt,
    net::SocketAddr,
    ops::ControlFlow,
    time::{Duration, Instant},
};

use http_body_util::BodyExt;
use hyper::{body::Incoming, http::response::Parts, StatusCode};
use mirrord_protocol::{
    batched_body::BatchedBody,
    tcp::{
        ChunkedRequestBodyV1, ChunkedRequestErrorV1, ChunkedResponse, HttpRequest,
        HttpRequestTransportType, HttpResponse, InternalHttpBody, InternalHttpBodyFrame,
        InternalHttpResponse,
    },
};
use tokio::time;
use tokio_retry::strategy::ExponentialBackoff;
use tracing::Level;

use super::{
    http::{mirrord_error_response, ClientStore, LocalHttpError, ResponseMode, StreamingBody},
    tasks::{HttpOut, InProxyTaskMessage},
};
use crate::background_tasks::{BackgroundTask, MessageBus};

/// [`BackgroundTask`] used by the [`IncomingProxy`](super::IncomingProxy).
///
/// Responsible for delivering a single HTTP request to the user application.
///
/// Exits immediately when it's [`TaskSender`](crate::background_tasks::TaskSender) is dropped.
pub struct HttpGatewayTask {
    /// Request to deliver.
    request: HttpRequest<StreamingBody>,
    /// Shared cache of [`LocalHttpClient`](super::http::LocalHttpClient)s.
    client_store: ClientStore,
    /// Determines response variant.
    response_mode: ResponseMode,
    /// Address of the HTTP server in the user application.
    server_addr: SocketAddr,
    /// How to transport the HTTP request to the server.
    transport: HttpRequestTransportType,
}

impl fmt::Debug for HttpGatewayTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpGatewayTask")
            .field("request", &self.request)
            .field("response_mode", &self.response_mode)
            .field("server_addr", &self.server_addr)
            .field("transport", &self.transport)
            .finish()
    }
}

impl HttpGatewayTask {
    /// Creates a new gateway task.
    pub fn new(
        request: HttpRequest<StreamingBody>,
        client_store: ClientStore,
        response_mode: ResponseMode,
        server_addr: SocketAddr,
        transport: HttpRequestTransportType,
    ) -> Self {
        Self {
            request,
            client_store,
            response_mode,
            server_addr,
            transport,
        }
    }

    /// Handles the response if we operate in [`ResponseMode::Chunked`].
    ///
    /// # Returns
    ///
    /// * An error if we failed before sending the [`ChunkedResponse::Start`] message through the
    ///   [`MessageBus`] (we can still retry the request)
    /// * [`ControlFlow::Break`] if we failed after sending the [`ChunkedResponse::Start`] message
    /// * [`ControlFlow::Continue`] if we succeeded
    #[tracing::instrument(level = Level::DEBUG, skip_all, ret, err(level = Level::WARN))]
    async fn handle_response_chunked(
        &self,
        parts: Parts,
        mut body: Incoming,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<ControlFlow<()>, LocalHttpError> {
        let frames = body
            .ready_frames()
            .map_err(From::from)
            .map_err(LocalHttpError::ReadBodyFailed)?;

        if frames.is_last {
            let ready_frames = frames
                .frames
                .into_iter()
                .map(InternalHttpBodyFrame::from)
                .collect::<VecDeque<_>>();

            tracing::debug!(
                ?ready_frames,
                "All response body frames were instantly ready, sending full response"
            );
            let response = HttpResponse {
                port: self.request.port,
                connection_id: self.request.connection_id,
                request_id: self.request.request_id,
                internal_response: InternalHttpResponse {
                    status: parts.status,
                    version: parts.version,
                    headers: parts.headers,
                    body: InternalHttpBody(ready_frames),
                },
            };
            message_bus.send(HttpOut::ResponseFramed(response)).await;

            return Ok(ControlFlow::Continue(()));
        }

        let ready_frames = frames
            .frames
            .into_iter()
            .map(InternalHttpBodyFrame::from)
            .collect::<Vec<_>>();
        tracing::trace!(
            ?ready_frames,
            "Some response body frames were instantly ready, \
            but response body may not be finished yet"
        );

        let response = HttpResponse {
            port: self.request.port,
            connection_id: self.request.connection_id,
            request_id: self.request.request_id,
            internal_response: InternalHttpResponse {
                status: parts.status,
                version: parts.version,
                headers: parts.headers,
                body: ready_frames,
            },
        };
        message_bus
            .send(HttpOut::ResponseChunked(ChunkedResponse::Start(response)))
            .await;

        loop {
            let start = Instant::now();
            match body.next_frames().await {
                Ok(frames) => {
                    let is_last = frames.is_last;
                    let frames = frames
                        .frames
                        .into_iter()
                        .map(InternalHttpBodyFrame::from)
                        .collect::<Vec<_>>();
                    tracing::trace!(
                        ?frames,
                        is_last,
                        elapsed_ms = start.elapsed().as_millis(),
                        "Received a next batch of response body frames",
                    );

                    message_bus
                        .send(HttpOut::ResponseChunked(ChunkedResponse::Body(
                            ChunkedRequestBodyV1 {
                                frames,
                                is_last,
                                connection_id: self.request.connection_id,
                                request_id: self.request.request_id,
                            },
                        )))
                        .await;

                    if is_last {
                        break;
                    }
                }

                // Do not return any error here, as it would later be transformed into an error
                // response. We already send the request head to the agent.
                Err(error) => {
                    tracing::warn!(
                        %error,
                        elapsed_ms = start.elapsed().as_millis(),
                        gateway = ?self,
                        "Failed to read next response body frames",
                    );

                    message_bus
                        .send(HttpOut::ResponseChunked(ChunkedResponse::Error(
                            ChunkedRequestErrorV1 {
                                connection_id: self.request.connection_id,
                                request_id: self.request.request_id,
                            },
                        )))
                        .await;

                    return Ok(ControlFlow::Break(()));
                }
            }
        }

        Ok(ControlFlow::Continue(()))
    }

    /// Makes an attempt to send the request and read the whole response.
    ///
    /// [`Err`] is handled in the caller and, if we run out of send attempts, converted to an error
    /// response. Because of this, this function should not return any error that happened after
    /// sending [`ChunkedResponse::Start`]. The agent would get a duplicated response.
    #[tracing::instrument(level = Level::DEBUG, skip_all, err(level = Level::WARN))]
    async fn send_attempt(&self, message_bus: &mut MessageBus<Self>) -> Result<(), LocalHttpError> {
        let mut client = self
            .client_store
            .get(
                self.server_addr,
                self.request.version(),
                &self.transport,
                &self.request.internal_request.uri,
            )
            .await?;
        let mut response = client.send_request(self.request.clone()).await?;
        let on_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS).then(|| {
            tracing::debug!("Detected an HTTP upgrade");
            hyper::upgrade::on(&mut response)
        });
        let (parts, body) = response.into_parts();

        let flow = match self.response_mode {
            ResponseMode::Basic => {
                let start = Instant::now();
                let body: Vec<u8> = body
                    .collect()
                    .await
                    .map_err(From::from)
                    .map_err(LocalHttpError::ReadBodyFailed)?
                    .to_bytes()
                    .into();
                tracing::debug!(
                    body_len = body.len(),
                    elapsed_ms = start.elapsed().as_millis(),
                    "Collected the whole response body",
                );

                let response = HttpResponse {
                    port: self.request.port,
                    connection_id: self.request.connection_id,
                    request_id: self.request.request_id,
                    internal_response: InternalHttpResponse {
                        status: parts.status,
                        version: parts.version,
                        headers: parts.headers,
                        body,
                    },
                };
                message_bus.send(HttpOut::ResponseBasic(response)).await;

                ControlFlow::Continue(())
            }
            ResponseMode::Framed => {
                let start = Instant::now();
                let body = InternalHttpBody::from_body(body)
                    .await
                    .map_err(From::from)
                    .map_err(LocalHttpError::ReadBodyFailed)?;
                tracing::debug!(
                    ?body,
                    elapsed_ms = start.elapsed().as_millis(),
                    "Collected the whole response body",
                );

                let response = HttpResponse {
                    port: self.request.port,
                    connection_id: self.request.connection_id,
                    request_id: self.request.request_id,
                    internal_response: InternalHttpResponse {
                        status: parts.status,
                        version: parts.version,
                        headers: parts.headers,
                        body,
                    },
                };
                message_bus.send(HttpOut::ResponseFramed(response)).await;

                ControlFlow::Continue(())
            }
            ResponseMode::Chunked => {
                self.handle_response_chunked(parts, body, message_bus)
                    .await?
            }
        };

        if flow.is_break() {
            return Ok(());
        }

        if let Some(on_upgrade) = on_upgrade {
            message_bus.send(HttpOut::Upgraded(on_upgrade)).await;
        } else {
            // If there was no upgrade and no error, the client can be reused.
            self.client_store.push_idle(client);
        }

        Ok(())
    }
}

impl BackgroundTask for HttpGatewayTask {
    type Error = Infallible;
    type MessageIn = Infallible;
    type MessageOut = InProxyTaskMessage;

    #[tracing::instrument(
        level = Level::INFO, name = "http_gateway_task_main_loop",
        skip(message_bus),
        ret, err(level = Level::WARN),
    )]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        // Will return 9 backoffs: 50ms, 100ms, 200ms, 400ms, 500ms, 500ms, ...
        let mut backoffs = ExponentialBackoff::from_millis(2)
            .factor(25)
            .max_delay(Duration::from_millis(500))
            .take(9);

        let guard = message_bus.closed();

        let mut attempt = 0;
        let error = loop {
            attempt += 1;
            tracing::debug!(attempt, "Starting send attempt");
            match guard.cancel_on_close(self.send_attempt(message_bus)).await {
                None | Some(Ok(())) => return Ok(()),
                Some(Err(error)) => {
                    let backoff = error.can_retry().then(|| backoffs.next()).flatten();

                    let Some(backoff) = backoff else {
                        tracing::warn!(
                            gateway = ?self,
                            failed_attempts = attempt,
                            error = %Report::new(&error),
                            "Failed to send an HTTP request",
                        );

                        break error;
                    };

                    tracing::trace!(
                        backoff_ms = backoff.as_millis(),
                        failed_attempts = attempt,
                        error = %Report::new(error),
                        "Trying again after backoff",
                    );

                    if guard.cancel_on_close(time::sleep(backoff)).await.is_none() {
                        return Ok(());
                    }
                }
            }
        };

        let response = mirrord_error_response(
            Report::new(error).pretty(true),
            self.request.version(),
            self.request.connection_id,
            self.request.request_id,
            self.request.port,
        );
        message_bus.send(HttpOut::ResponseBasic(response)).await;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{io, pin::Pin, sync::Arc};

    use bytes::Bytes;
    use http_body_util::{Empty, StreamBody};
    use hyper::{
        body::{Frame, Incoming},
        header::{self, HeaderValue, CONNECTION, UPGRADE},
        server::conn::http1,
        service::service_fn,
        upgrade::Upgraded,
        Method, Request, Response, StatusCode, Version,
    };
    use hyper_util::rt::TokioIo;
    use mirrord_protocol::{
        tcp::{HttpRequest, InternalHttpRequest},
        ConnectionId,
    };
    use rstest::rstest;
    use rustls::ServerConfig;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{mpsc, watch, Semaphore},
        task,
    };
    use tokio_rustls::TlsAcceptor;
    use tokio_stream::wrappers::ReceiverStream;

    use super::*;
    use crate::{
        background_tasks::{BackgroundTasks, TaskUpdate},
        proxies::incoming::{
            tcp_proxy::{LocalTcpConnection, TcpProxyTask},
            InProxyTaskError,
        },
    };

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
    async fn dummy_echo_server(
        listener: TcpListener,
        acceptor: Option<TlsAcceptor>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            let (stream, _) = tokio::select! {
                res = listener.accept() => res.expect("dummy echo server failed to accept a TCP connection"),
                _ = shutdown.changed() => break,
            };

            let mut shutdown = shutdown.clone();

            match acceptor.clone() {
                Some(acceptor) => {
                    task::spawn(async move {
                        let stream = acceptor
                            .accept(stream)
                            .await
                            .expect("dummy echo server failed to accept a TLS connection");
                        let conn = http1::Builder::new().serve_connection(
                            TokioIo::new(stream),
                            service_fn(upgrade_req_handler),
                        );
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

                None => {
                    task::spawn(async move {
                        let conn = http1::Builder::new().serve_connection(
                            TokioIo::new(stream),
                            service_fn(upgrade_req_handler),
                        );
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
            }
        }
    }

    /// Verifies that [`HttpGatewayTask`] and [`TcpProxyTask`] together correctly handle HTTP
    /// upgrades.
    #[rstest]
    #[case::with_tls(true)]
    #[case::without_tls(false)]
    #[tokio::test]
    async fn handles_http_upgrades(#[case] use_tls: bool) {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::aws_lc_rs::default_provider(),
        );

        let acceptor = use_tls.then(|| {
            let root = mirrord_tls_util::generate_cert("root", None, true).unwrap();
            let server = mirrord_tls_util::generate_cert("server", None, false).unwrap();
            let mut config = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![server.cert.into(), root.cert.into()],
                    server.key_pair.serialize_der().try_into().unwrap(),
                )
                .unwrap();
            config.alpn_protocols = vec![b"http/1.1".into()];
            TlsAcceptor::from(Arc::new(config))
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server_task = task::spawn(dummy_echo_server(listener, acceptor, shutdown_rx));

        let mut tasks: BackgroundTasks<ConnectionId, InProxyTaskMessage, InProxyTaskError> =
            Default::default();
        let _gateway = {
            let request = HttpRequest {
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
            };
            let gateway = HttpGatewayTask::new(
                request,
                ClientStore::new_with_timeout(Duration::from_secs(1), Default::default()),
                ResponseMode::Basic,
                local_destination,
                if use_tls {
                    HttpRequestTransportType::Tls {
                        alpn_protocol: Some(b"http/1.1".into()),
                        server_name: None,
                    }
                } else {
                    HttpRequestTransportType::Tcp
                },
            );
            tasks.register(gateway, 0, 8)
        };

        let message = tasks
            .next()
            .await
            .expect("no task result")
            .1
            .unwrap_message();
        match message {
            InProxyTaskMessage::Http(HttpOut::ResponseBasic(res)) => {
                assert_eq!(
                    res.internal_response.status,
                    StatusCode::SWITCHING_PROTOCOLS
                );
                println!("Received response from the gateway: {res:?}");
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
            other => panic!("unexpected task update: {other:?}"),
        }

        let message = tasks
            .next()
            .await
            .expect("not task result")
            .1
            .unwrap_message();
        let on_upgrade = match message {
            InProxyTaskMessage::Http(HttpOut::Upgraded(on_upgrade)) => on_upgrade,
            other => panic!("unexpected task update: {other:?}"),
        };
        let update = tasks.next().await.expect("no task result");
        match update.1 {
            TaskUpdate::Finished(Ok(())) => {}
            other => panic!("unexpected task update: {other:?}"),
        }

        let proxy = tasks.register(
            TcpProxyTask::new(
                update.0,
                LocalTcpConnection::AfterUpgrade(on_upgrade),
                false,
            ),
            1,
            8,
        );

        proxy.send(b"test test test".to_vec()).await;

        let message = tasks
            .next()
            .await
            .expect("no task result")
            .1
            .unwrap_message();
        match message {
            InProxyTaskMessage::Tcp(bytes) => {
                assert_eq!(bytes, INITIAL_MESSAGE);
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let message = tasks
            .next()
            .await
            .expect("no task result")
            .1
            .unwrap_message();
        match message {
            InProxyTaskMessage::Tcp(bytes) => {
                assert_eq!(bytes, b"test test test");
            }
            _ => panic!("unexpected task update: {update:?}"),
        }

        let _ = shutdown_tx.send(true);
        server_task.await.expect("dummy echo server panicked");
    }

    /// Verifies that [`HttpGatewayTask`] produces correct variant of the [`HttpResponse`].
    ///
    /// Verifies that body of
    /// [`LayerTcpSteal::HttpResponseChunked`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponseChunked)
    /// is streamed.
    #[rstest]
    #[case::basic(ResponseMode::Basic)]
    #[case::framed(ResponseMode::Framed)]
    #[case::chunked(ResponseMode::Chunked)]
    #[tokio::test]
    async fn produces_correct_response_variant(#[case] response_mode: ResponseMode) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let semaphore: Arc<Semaphore> = Arc::new(Semaphore::const_new(0));
        let semaphore_clone = semaphore.clone();

        let conn_task = tokio::spawn(async move {
            let service = service_fn(|_req: Request<Incoming>| {
                let semaphore = semaphore_clone.clone();
                async move {
                    let (frame_tx, frame_rx) = mpsc::channel::<hyper::Result<Frame<Bytes>>>(1);

                    tokio::spawn(async move {
                        for _ in 0..2 {
                            semaphore.acquire().await.unwrap().forget();
                            let _ = frame_tx
                                .send(Ok(Frame::data(Bytes::from_static(b"hello\n"))))
                                .await;
                        }
                    });

                    let body = StreamBody::new(ReceiverStream::new(frame_rx));
                    let mut response = Response::new(body);
                    response
                        .headers_mut()
                        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("12"));

                    Ok::<_, Infallible>(response)
                }
            });

            let (connection, _) = listener.accept().await.unwrap();
            http1::Builder::new()
                .serve_connection(TokioIo::new(connection), service)
                .await
                .unwrap()
        });

        let request = HttpRequest {
            connection_id: 0,
            request_id: 0,
            port: 80,
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: Version::HTTP_11,
                body: StreamingBody::from(Vec::<u8>::new()),
            },
        };

        let mut tasks: BackgroundTasks<(), InProxyTaskMessage, Infallible> = Default::default();
        let _gateway = tasks.register(
            HttpGatewayTask::new(
                request,
                ClientStore::new_with_timeout(Duration::from_secs(1), Default::default()),
                response_mode,
                addr,
                HttpRequestTransportType::Tcp,
            ),
            (),
            8,
        );

        match response_mode {
            ResponseMode::Basic => {
                semaphore.add_permits(2);
                match tasks.next().await.unwrap().1.unwrap_message() {
                    InProxyTaskMessage::Http(HttpOut::ResponseBasic(response)) => {
                        assert_eq!(response.internal_response.body, b"hello\nhello\n");
                    }
                    other => panic!("unexpected task message: {other:?}"),
                }
            }

            ResponseMode::Framed => {
                semaphore.add_permits(2);
                match tasks.next().await.unwrap().1.unwrap_message() {
                    InProxyTaskMessage::Http(HttpOut::ResponseFramed(response)) => {
                        let mut collected = vec![];
                        for frame in response.internal_response.body.0 {
                            match frame {
                                InternalHttpBodyFrame::Data(data) => collected.extend(data),
                                InternalHttpBodyFrame::Trailers(trailers) => {
                                    panic!("unexpected trailing headers: {trailers:?}");
                                }
                            }
                        }

                        assert_eq!(collected, b"hello\nhello\n");
                    }
                    other => panic!("unexpected task message: {other:?}"),
                }
            }

            ResponseMode::Chunked => {
                match tasks.next().await.unwrap().1.unwrap_message() {
                    InProxyTaskMessage::Http(HttpOut::ResponseChunked(ChunkedResponse::Start(
                        response,
                    ))) => {
                        assert!(response.internal_response.body.is_empty());
                    }
                    other => panic!("unexpected task message: {other:?}"),
                }

                semaphore.add_permits(1);
                match tasks.next().await.unwrap().1.unwrap_message() {
                    InProxyTaskMessage::Http(HttpOut::ResponseChunked(ChunkedResponse::Body(
                        body,
                    ))) => {
                        assert_eq!(
                            body.frames,
                            vec![InternalHttpBodyFrame::Data(b"hello\n".into())],
                        );
                        assert!(!body.is_last);
                    }
                    other => panic!("unexpected task message: {other:?}"),
                }

                semaphore.add_permits(1);
                match tasks.next().await.unwrap().1.unwrap_message() {
                    InProxyTaskMessage::Http(HttpOut::ResponseChunked(ChunkedResponse::Body(
                        body,
                    ))) => {
                        assert_eq!(
                            body.frames,
                            vec![InternalHttpBodyFrame::Data(b"hello\n".into())],
                        );
                        assert!(body.is_last);
                    }
                    other => panic!("unexpected task message: {other:?}"),
                }
            }
        }

        match tasks.next().await.unwrap().1 {
            TaskUpdate::Finished(Ok(())) => {}
            other => panic!("unexpected task update: {other:?}"),
        }

        conn_task.await.unwrap();
    }

    /// Verifies that [`HttpGateway`] sends request body frames to the server as soon as they are
    /// available.
    #[tokio::test]
    async fn streams_request_body_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let semaphore: Arc<Semaphore> = Arc::new(Semaphore::const_new(0));
        let semaphore_clone = semaphore.clone();

        let conn_task = tokio::spawn(async move {
            let service = service_fn(|mut req: Request<Incoming>| {
                let semaphore = semaphore_clone.clone();
                async move {
                    for _ in 0..2 {
                        semaphore.add_permits(1);
                        let frame = req
                            .body_mut()
                            .frame()
                            .await
                            .unwrap()
                            .unwrap()
                            .into_data()
                            .unwrap();
                        assert_eq!(frame, "hello\n");
                    }

                    Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new()))
                }
            });

            let (connection, _) = listener.accept().await.unwrap();
            http1::Builder::new()
                .serve_connection(TokioIo::new(connection), service)
                .await
                .unwrap()
        });

        let (frame_tx, frame_rx) = mpsc::channel(1);
        let body = StreamingBody::new(frame_rx, vec![]);
        let mut request = HttpRequest {
            connection_id: 0,
            request_id: 0,
            port: 80,
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: Version::HTTP_11,
                body,
            },
        };
        request
            .internal_request
            .headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("12"));

        let mut tasks: BackgroundTasks<(), InProxyTaskMessage, Infallible> = Default::default();
        let client_store =
            ClientStore::new_with_timeout(Duration::from_secs(1), Default::default());
        let _gateway = tasks.register(
            HttpGatewayTask::new(
                request,
                client_store.clone(),
                ResponseMode::Basic,
                addr,
                HttpRequestTransportType::Tcp,
            ),
            (),
            8,
        );

        for _ in 0..2 {
            semaphore.acquire().await.unwrap().forget();
            frame_tx
                .send(InternalHttpBodyFrame::Data(b"hello\n".into()))
                .await
                .unwrap();
        }
        std::mem::drop(frame_tx);

        match tasks.next().await.unwrap().1.unwrap_message() {
            InProxyTaskMessage::Http(HttpOut::ResponseBasic(response)) => {
                assert_eq!(response.internal_response.status, StatusCode::OK);
            }
            other => panic!("unexpected message: {other:?}"),
        }

        match tasks.next().await.unwrap().1 {
            TaskUpdate::Finished(Ok(())) => {}
            other => panic!("unexpected task update: {other:?}"),
        }

        conn_task.await.unwrap();
    }

    /// Verifies that [`HttpGateway`] reuses already established HTTP connections.
    #[tokio::test]
    async fn reuses_client_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let service = service_fn(|_req: Request<Incoming>| {
                std::future::ready(Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())))
            });

            let (connection, _) = listener.accept().await.unwrap();
            std::mem::drop(listener);
            http1::Builder::new()
                .serve_connection(TokioIo::new(connection), service)
                .await
                .unwrap()
        });

        let mut request = HttpRequest {
            connection_id: 0,
            request_id: 0,
            port: 80,
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: Version::HTTP_11,
                body: Default::default(),
            },
        };
        request
            .internal_request
            .headers
            .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));

        let mut tasks: BackgroundTasks<u32, InProxyTaskMessage, Infallible> = Default::default();
        let client_store =
            ClientStore::new_with_timeout(Duration::from_secs(1337 * 21 * 37), Default::default());
        let _gateway_1 = tasks.register(
            HttpGatewayTask::new(
                request.clone(),
                client_store.clone(),
                ResponseMode::Basic,
                addr,
                HttpRequestTransportType::Tcp,
            ),
            0,
            8,
        );
        let _gateway_2 = tasks.register(
            HttpGatewayTask::new(
                request.clone(),
                client_store.clone(),
                ResponseMode::Basic,
                addr,
                HttpRequestTransportType::Tcp,
            ),
            1,
            8,
        );

        let mut finished = 0;
        let mut responses = 0;

        while finished < 2 && responses < 2 {
            match tasks.next().await.unwrap() {
                (id, TaskUpdate::Finished(Ok(()))) => {
                    println!("gateway {id} finished");
                    finished += 1;
                }
                (
                    id,
                    TaskUpdate::Message(InProxyTaskMessage::Http(HttpOut::ResponseBasic(response))),
                ) => {
                    println!("gateway {id} returned a response");
                    assert_eq!(response.internal_response.status, StatusCode::OK);
                    responses += 1;
                }
                other => panic!("unexpected task update: {other:?}"),
            }
        }
    }
}
