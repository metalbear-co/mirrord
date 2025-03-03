use std::{
    collections::HashMap, future::Future, marker::PhantomData, net::SocketAddr, ops::Not, pin::Pin,
};

use bytes::Bytes;
use http::{header::UPGRADE, Version};
use http_body_util::{combinators::BoxBody, BodyExt};
use hyper::{
    body::Incoming,
    client::conn::{http1, http2},
    http::{Request, StatusCode},
    service::Service,
    upgrade::{OnUpgrade, Upgraded},
    Response,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::{
    tcp::{
        HttpRequestMetadata, HttpRequestTransportType, HTTP_CHUNKED_REQUEST_V2_VERSION,
        HTTP_FILTERED_UPGRADE_VERSION,
    },
    ConnectionId, LogMessage, RequestId,
};
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    task::{self, JoinHandle},
};
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::Level;

use super::{
    original_destination::OriginalDestination, ConnectionMessageIn, ConnectionMessageOut,
    ConnectionTaskError, STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION,
};
use crate::{
    http::HttpVersion,
    metrics::STEAL_FILTERED_CONNECTION_SUBSCRIPTION,
    steal::{connections::unfiltered::UnfilteredStealTask, subscriptions::Filters},
    util::ClientId,
};

/// [`Body`](hyper::body::Body) type used in [`FilteredStealTask`].
pub type DynamicBody = BoxBody<Bytes, hyper::Error>;

/// Incoming [`Request`] extracted from the HTTP connection in the [`FilteringService`].
struct ExtractedRequest {
    request: Request<Incoming>,
    response_tx: oneshot::Sender<RequestHandling>,
}

/// Response instruction for [`FilteringService`].
/// Sent from [`FilteredStealTask`] in [`ExtractedRequest::response_tx`].
enum RequestHandling {
    /// The [`Request`] should be handled by the original HTTP server.
    LetThrough { unchanged: Request<Incoming> },
    /// The [`FilteringService`] should respond immediately with the given [`Response`]
    /// on behalf of the given stealer client.
    RespondWith {
        response: Response<DynamicBody>,
        for_client: ClientId,
    },
}

/// HTTP server side of an upgraded connection retrieved from [`FilteringService`].
pub enum UpgradedServerSide {
    /// Stealer client. Their [`HttpFilter`](crate::steal::http::HttpFilter) matched the upgrade
    /// request. The rest of the connection should be proxied between the HTTP client and this
    /// stealer client (which acts as an HTTP server).
    MatchedClient(ClientId),
    /// TCP connection with the HTTP server that was the original destination of the HTTP client
    /// (no [`HttpFilter`](crate::steal::http::HttpFilter) matched the upgrade request). The rest
    /// of the connection should be proxied between the HTTP client and this HTTP server.
    OriginalDestination(Upgraded),
}

/// Two connections recovered after an HTTP upgrade that happened in [`FilteringService`].
pub struct UpgradedConnection {
    /// Client side - an HTTP client that tried to connect with agent's target.
    pub http_client_io: Upgraded,
    /// Server side - a stealer's client or an HTTP server running in the agent's target.
    pub http_server_io: UpgradedServerSide,
}

/// Simple [`Service`] implementor that uses [`mpsc`] channels to pass incoming [`Request`]s to a
/// [`FilteredStealTask`].
#[derive(Clone)]
struct FilteringService {
    /// For sending incoming requests to the [`FilteredStealTask`].
    requests_tx: Sender<ExtractedRequest>,

    /// For recovering the upgraded connection in [`FilteredStealTask`].
    ///
    /// # Note
    ///
    /// At most one value will **always** be sent through this channel (only one http upgrade is
    /// possible). However, using a [`oneshot`] here would require a combination of an
    /// [`Arc`](std::sync::Arc), a [`Mutex`](std::sync::Mutex) and an [`Option`]. [`mpsc`] is
    /// used here for simplicity.
    upgrade_tx: Sender<UpgradedConnection>,

    /// Original destination of the stolen requests.
    original_destination: OriginalDestination,
}

impl FilteringService {
    /// Produces a new [`StatusCode::BAD_GATEWAY`] [`Response`] with the given [`Version`] and the
    /// given `error` in body.
    fn bad_gateway(version: Version, error: &str) -> Response<DynamicBody> {
        let body = format!("mirrord: {error}");

        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .version(version)
            .body(BoxBody::new(body.map_err(|_| unreachable!())))
            .expect("creating an empty response should not fail")
    }

    /// Sends the given [`Request`] to the original destination.
    ///
    /// # TODO
    ///
    /// This method always creates a new TCP connection and preforms an HTTP handshake.
    /// Also, it does not retry the request upon failure.
    async fn send_request(
        &self,
        mut request: Request<Incoming>,
    ) -> Result<Response<Incoming>, Box<dyn std::error::Error>> {
        let stream = self
            .original_destination
            .connect(request.uri())
            .await
            .inspect_err(|error| {
                tracing::error!(
                    %error,
                    destination = ?self.original_destination,
                    "Failed to connect to the request original destination HTTP server",
                );
            })?;

        match request.version() {
            Version::HTTP_2 => {
                let (mut request_sender, connection) =
                    http2::handshake(TokioExecutor::default(), TokioIo::new(stream))
                        .await
                        .inspect_err(|error| {
                            tracing::error!(
                                ?error,
                                "HTTP2 handshake with the original destination failed"
                            )
                        })?;

                // We need this to progress the connection forward (hyper thing).
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::error!(?error, "Connection with the original destination failed");
                    }
                });

                // fixes https://github.com/metalbear-co/mirrord/issues/2497
                // inspired by https://github.com/linkerd/linkerd2-proxy/blob/c5d9f1c1e7b7dddd9d75c0d1a0dca68188f38f34/linkerd/proxy/http/src/h2.rs#L175
                if request.uri().authority().is_none() {
                    *request.version_mut() = hyper::http::Version::HTTP_11;
                }

                request_sender
                    .send_request(request)
                    .await
                    .map_err(Into::into)
            }

            _ => {
                let (mut request_sender, connection) = http1::handshake(TokioIo::new(stream))
                    .await
                    .inspect_err(|error| {
                        tracing::error!(
                            ?error,
                            "HTTP1 handshake with the original destination failed"
                        )
                    })?;

                // We need this to progress the connection forward (hyper thing).
                tokio::spawn(async move {
                    if let Err(error) = connection.with_upgrades().await {
                        tracing::error!(?error, "Connection with the original destination failed");
                    }
                });

                request_sender
                    .send_request(request)
                    .await
                    .map_err(Into::into)
            }
        }
    }

    /// Sends the given [`Request`] to the destination given as `to`. If the [`Response`] is
    /// [`StatusCode::SWITCHING_PROTOCOLS`], spawns a new [`tokio::task`] to await for the given
    /// [`OnUpgrade`].
    #[tracing::instrument(
        level = "trace",
        name = "let_http_request_through",
        skip(self, request, on_upgrade),
        fields(
            request_path = request.uri().path(),
            request_headers = ?request.headers(),
        )
    )]
    async fn let_through(
        &self,
        request: Request<Incoming>,
        on_upgrade: OnUpgrade,
    ) -> Response<DynamicBody> {
        let version = request.version();
        let mut response = self
            .send_request(request)
            .await
            .map(|response| response.map(BoxBody::new))
            .unwrap_or_else(|_| {
                Self::bad_gateway(
                    version,
                    "failed to pass the request to its original destination",
                )
            });

        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let http_server_on_upgrade = hyper::upgrade::on(&mut response);
            let upgrade_tx = self.upgrade_tx.clone();

            task::spawn(async move {
                let Ok((upgraded_client, upgraded_server)) =
                    tokio::try_join!(on_upgrade, http_server_on_upgrade)
                        .inspect_err(|error| tracing::trace!(?error, "HTTP upgrade failed"))
                else {
                    return;
                };

                let res = upgrade_tx
                    .send(UpgradedConnection {
                        http_client_io: upgraded_client,
                        http_server_io: UpgradedServerSide::OriginalDestination(upgraded_server),
                    })
                    .await;

                if res.is_err() {
                    tracing::trace!("HTTP upgrade lost - channel closed");
                }
            });
        }

        response
    }

    /// Checks whether the given [`Response`] is [`StatusCode::SWITCHING_PROTOCOLS`].
    /// If this is the case, spawns new [`tokio::task`] to await on the given [`OnUpgrade`].
    /// Assumes that the [`Response`] comes from the stealer client given as `from_client`
    /// and the given `on_upgrade` comes from the original [`Request`].
    async fn check_protocol_switch(
        &self,
        response: &Response<DynamicBody>,
        on_upgrade: OnUpgrade,
        from_client: ClientId,
    ) {
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let upgrade_tx = self.upgrade_tx.clone();

            task::spawn(async move {
                let Ok(upgraded) = on_upgrade.await.inspect_err(|error| {
                    tracing::trace!(?error, client_id = from_client, "HTTP upgrade failed")
                }) else {
                    return;
                };

                let res = upgrade_tx
                    .send(UpgradedConnection {
                        http_client_io: upgraded,
                        http_server_io: UpgradedServerSide::MatchedClient(from_client),
                    })
                    .await;

                if res.is_err() {
                    tracing::trace!(
                        client_id = from_client,
                        "HTTP upgrade lost - channel closed"
                    );
                }
            });
        }
    }

    /// Extracts [`OnUpgrade`] from the given [`Request`] and sends it to [`FilteredStealTask`].
    /// Waits on a dynamically created [`oneshot::channel`] for [`RequestHandling`] instruction.
    async fn handle_request(
        &self,
        mut request: Request<Incoming>,
    ) -> Result<Response<DynamicBody>, ConnectionTaskError> {
        let version = request.version();
        let on_upgrade = hyper::upgrade::on(&mut request);

        let (response_tx, response_rx) = oneshot::channel();
        self.requests_tx
            .send(ExtractedRequest {
                request,
                response_tx,
            })
            .await?;

        let response = match response_rx.await {
            Ok(RequestHandling::LetThrough { unchanged }) => {
                self.let_through(unchanged, on_upgrade).await
            }
            Ok(RequestHandling::RespondWith {
                response,
                for_client,
            }) => {
                self.check_protocol_switch(&response, on_upgrade, for_client)
                    .await;
                response
            }
            Err(..) => Self::bad_gateway(
                version,
                "failed to receive a response from the connected mirrord session",
            ),
        };

        Ok(response)
    }
}

impl Service<Request<Incoming>> for FilteringService {
    type Response = Response<BoxBody<Bytes, hyper::Error>>;

    type Error = ConnectionTaskError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let service = self.clone();

        Box::pin(async move { service.handle_request(request).await })
    }
}

/// Manages a filtered stolen connection.
///
/// Uses a custom [`Service`] implementation to handle HTTP protocol details, change the raw IO
/// stream into a series requests and provide responses.
pub struct FilteredStealTask<T> {
    connection_id: ConnectionId,
    /// Original destination of the stolen connection.
    original_destination: OriginalDestination,
    /// Address of connection peer.
    peer_address: SocketAddr,

    /// Stealer client to [`HttpFilter`](crate::steal::http::HttpFilter) mapping. Allows for
    /// routing HTTP requests to correct stealer clients.
    ///
    /// # Note
    ///
    /// This mapping is shared via [`Arc`](std::sync::Arc), allowing for dynamic updates from the
    /// outside. This allows for *injecting* new stealer clients into exisiting connections.
    filters: Filters,

    /// Stealer client to subscription state mapping.
    /// 1. `true` -> client is subscribed
    /// 2. `false` -> client has unsubscribed or we sent [`ConnectionMessageOut::Closed`].
    /// 3. `None` -> client does not know about this connection at all
    ///
    /// Used to send [`ConnectionMessageOut::Closed`] in [`Self::run`] when
    /// [`Self::run_until_http_ends`] and [`Self::run_after_http_ends`] have finished.
    subscribed: HashMap<ClientId, bool>,

    /// For receiving [`Request`]s extracted by the [`FilteringService`].
    requests_rx: Receiver<ExtractedRequest>,

    /// 1. Handle for the [`tokio::task`] that polls the [`hyper`] connection of related
    ///    [`FilteringService`].
    /// 2. [`DropGuard`] for this [`tokio::task`], so that it aborts when this struct is dropped.
    hyper_conn_task: Option<(JoinHandle<Option<UpgradedConnection>>, DropGuard)>,

    /// Requests blocked on stealer clients' responses.
    blocked_requests: HashMap<(ClientId, RequestId), oneshot::Sender<RequestHandling>>,

    /// Id of the next HTTP request that will be intercepted.
    next_request_id: RequestId,

    /// For safely downcasting the IO stream after an HTTP upgrade. See [`Upgraded::downcast`].
    _io_type: PhantomData<fn() -> T>,

    /// Helps us figuring out if we should update some metrics in the `Drop` implementation.
    metrics_updated: bool,
}

impl<T> Drop for FilteredStealTask<T> {
    fn drop(&mut self) {
        if self.metrics_updated.not() {
            STEAL_FILTERED_CONNECTION_SUBSCRIPTION
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl<T> FilteredStealTask<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Limits the number requests served concurrently by [`FilteringService`].
    const MAX_CONCURRENT_REQUESTS: usize = 128;

    /// Creates a new instance of this task. The task will manage the connection given as `io` and
    /// use the provided `filters` for matching incoming [`Request`]s with stealing clients.
    ///
    /// The task will not run yet, see [`Self::run`].
    #[tracing::instrument(
        level = "trace",
        name = "create_new_filtered_steal_task",
        skip(filters, io)
    )]
    pub fn new(
        connection_id: ConnectionId,
        filters: Filters,
        original_destination: OriginalDestination,
        peer_address: SocketAddr,
        http_version: HttpVersion,
        io: T,
    ) -> Self {
        let (upgrade_tx, mut upgrade_rx) = mpsc::channel(1);
        let (requests_tx, requests_rx) = mpsc::channel(Self::MAX_CONCURRENT_REQUESTS);

        let service = FilteringService {
            requests_tx,
            upgrade_tx,
            original_destination: original_destination.clone(),
        };

        let cancellation_token = CancellationToken::new();
        let drop_guard = cancellation_token.clone().drop_guard();
        let cancelled = cancellation_token.cancelled_owned();

        let task_handle = match http_version {
            HttpVersion::V1 => {
                let conn = hyper::server::conn::http1::Builder::new()
                    .preserve_header_case(true)
                    .serve_connection(TokioIo::new(io), service)
                    .with_upgrades();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancelled => None,
                        res = conn => match res {
                            Err(error) => {
                                tracing::error!(?error, "HTTP connection failed");
                                None
                            }
                            Ok(()) => upgrade_rx.recv().await,
                        },
                    }
                })
            }

            HttpVersion::V2 => {
                let conn = hyper::server::conn::http2::Builder::new(TokioExecutor::default())
                    .serve_connection(TokioIo::new(io), service);
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancelled => None,
                        res = conn => match res {
                            Err(error) => {
                                tracing::error!(?error, "HTTP connection failed");
                                None
                            }
                            Ok(()) => upgrade_rx.recv().await,
                        },
                    }
                })
            }
        };

        STEAL_FILTERED_CONNECTION_SUBSCRIPTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Self {
            connection_id,
            original_destination,
            peer_address,
            filters,
            subscribed: Default::default(),
            requests_rx,
            hyper_conn_task: Some((task_handle, drop_guard)),
            blocked_requests: Default::default(),
            next_request_id: Default::default(),
            _io_type: Default::default(),
            metrics_updated: false,
        }
    }

    /// Matches the given [`Request`] against [`Self::filters`] and state of [`Self::subscribed`].
    #[tracing::instrument(
        level = Level::TRACE,
        name = "match_request_with_filter",
        skip(self, request),
        fields(
            request_path = request.uri().path(),
            request_headers = ?request.headers(),
            filters = ?self.filters,
        )
        ret,
    )]
    fn match_request<B>(&self, request: &mut Request<B>) -> (Option<ClientId>, Vec<ClientId>) {
        let protocol_version_req = self
            .original_destination
            .connector()
            .is_some()
            .then_some(&*HTTP_CHUNKED_REQUEST_V2_VERSION)
            .or_else(|| {
                request
                    .headers()
                    .contains_key(UPGRADE)
                    .then_some(&*HTTP_FILTERED_UPGRADE_VERSION)
            });

        let mut iter = self
            .filters
            .iter()
            // Check if the client can handle the request.
            .filter(|entry| {
                let Some(req) = &protocol_version_req else {
                    return true;
                };

                entry.value().1.as_ref().is_some_and(|v| req.matches(v))
            })
            // Check if the client's filter matches the request.
            .filter(|entry| entry.value().0.matches(request))
            .map(|entry| *entry.key());

        let first_match =
            iter.find(|client_id| self.subscribed.get(client_id).copied().unwrap_or(true));
        let other_matches = iter
            .filter(|client_id| self.subscribed.get(client_id).copied().unwrap_or(true))
            .collect();

        (first_match, other_matches)
    }

    /// Sends the given [`Response`] to the [`FilteringService`] via [`oneshot::Sender`] from
    /// [`Self::blocked_requests`].
    ///
    /// If there is no blocked request for the given ([`ClientId`], [`RequestId`]) combination or
    /// the HTTP connection is dead, does nothing.
    #[tracing::instrument(
        level = Level::TRACE,
        name = "handle_filtered_request_response",
        skip(self, response),
        fields(
            status = u16::from(response.status()),
            connection_id = self.connection_id,
            original_destination = %self.original_destination.address(),
        )
    )]
    fn handle_response(
        &mut self,
        client_id: ClientId,
        request_id: RequestId,
        response: Response<DynamicBody>,
    ) {
        let Some(tx) = self.blocked_requests.remove(&(client_id, request_id)) else {
            tracing::warn!(
                client_id,
                request_id,
                ?response,
                connection_id = self.connection_id,
                "Received a response for an unexpected (client_id, request_id) combination",
            );

            return;
        };

        if tx
            .send(RequestHandling::RespondWith {
                response,
                for_client: client_id,
            })
            .is_err()
        {
            tracing::warn!(
                client_id,
                request_id,
                connection_id = self.connection_id,
                "Failed to send a response - HTTP connection is probably closed",
            );
        }
    }

    /// Notifies the [`FilteringService`] that the client failed to provide a [`Response`] for the
    /// request with the given id. The [`FilteringService`] is notified by dropping a
    /// [`oneshot::Sender`] from [`Self::blocked_requests`].
    ///
    /// If there is no blocked request for the given ([`ClientId`], [`RequestId`]) combination or
    /// the HTTP connection is dead, does nothing.
    #[tracing::instrument(
        level = Level::TRACE,
        name = "handle_filtered_request_response_failure",
        skip(self),
        fields(
            connection_id = self.connection_id,
            original_destination = %self.original_destination.address(),
        )
    )]
    fn handle_response_failure(&mut self, client_id: ClientId, request_id: RequestId) {
        let removed = self
            .blocked_requests
            .remove(&(client_id, request_id))
            .is_some();
        if !removed {
            tracing::warn!(
                client_id,
                request_id,
                connection_id = self.connection_id,
                "Received a response failure for an unexpected (client_id, request_id) combination",
            );
        }
    }

    /// Handles a [`Request`] intercepted by the [`FilteringService`].
    #[tracing::instrument(
        level = Level::TRACE,
        skip(self, tx),
        fields(?request = request.request),
        err(level = Level::WARN)
    )]
    async fn handle_request(
        &mut self,
        mut request: ExtractedRequest,
        tx: &Sender<ConnectionMessageOut>,
    ) -> Result<(), ConnectionTaskError> {
        let (Some(client_id), other_client_ids) = self.match_request(&mut request.request) else {
            let _ = request.response_tx.send(RequestHandling::LetThrough {
                unchanged: request.request,
            });

            return Ok(());
        };

        if self.subscribed.insert(client_id, true).is_none() {
            // First time this client will receive a request from this connection.
            tx.send(ConnectionMessageOut::SubscribedHttp {
                client_id,
                connection_id: self.connection_id,
            })
            .await?;
        }

        let id = self.next_request_id;
        self.next_request_id += 1;

        let transport = match self.original_destination.connector() {
            Some(connector) => HttpRequestTransportType::Tls {
                alpn_protocol: connector.alpn_protocol().map(Vec::from),
                server_name: connector.server_name().map(|name| name.to_str().into()),
            },
            None => HttpRequestTransportType::Tcp,
        };

        tx.send(ConnectionMessageOut::Request {
            client_id,
            connection_id: self.connection_id,
            request: request.request,
            id,
            metadata: HttpRequestMetadata::V1 {
                destination: self.original_destination.address(),
                source: self.peer_address,
            },
            transport,
        })
        .await?;

        self.blocked_requests
            .insert((client_id, id), request.response_tx);

        for client_id in other_client_ids {
            tx.send(ConnectionMessageOut::LogMessage {
                client_id,
                connection_id: self.connection_id,
                message: LogMessage::warn(format!(
                    "An HTTP request was stolen by another user. URI=({:?}), HEADERS=({:?})",
                    request.request.uri(),
                    request.request.headers(),
                )),
            })
            .await?;
        }

        Ok(())
    }

    /// Runs this task until the HTTP connection is closed or upgraded.
    ///
    /// # Returns
    ///
    /// Returns raw data sent by stealer clients after all their HTTP responses (to catch the case
    /// when the client starts sending data immediately after the `101 SWITCHING PROTOCOLS`).
    #[tracing::instrument(level = Level::TRACE, skip_all, ret, err)]
    async fn run_until_http_ends(
        &mut self,
        tx: Sender<ConnectionMessageOut>,
        rx: &mut Receiver<ConnectionMessageIn>,
    ) -> Result<HashMap<ClientId, Vec<Vec<u8>>>, ConnectionTaskError> {
        // Raw data that was received before we moved to the after-upgrade phase.
        // The stealer client might start sending raw bytes immediately after the `101 SWITCHING
        // PROTOCOLS` response.
        let mut queued_raw_data: HashMap<ClientId, Vec<Vec<u8>>> = Default::default();

        loop {
            tokio::select! {
                message = rx.recv() => match message.ok_or(ConnectionTaskError::RecvError)? {
                    ConnectionMessageIn::Raw { data, client_id } => {
                        tracing::trace!(client_id, connection_id = self.connection_id, "Received raw data");
                        queued_raw_data.entry(client_id).or_default().push(data);
                    },
                    ConnectionMessageIn::Response { response, request_id, client_id } => {
                        queued_raw_data.remove(&client_id);
                        self.handle_response(client_id, request_id, response);
                    },
                    ConnectionMessageIn::Unsubscribed { client_id } => {
                        queued_raw_data.remove(&client_id);
                        self.subscribed.insert(client_id, false);
                        self.blocked_requests.retain(|key, _| key.0 != client_id);

                        STEAL_FILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    },
                },

                request = self.requests_rx.recv() => match request {
                    Some(request) => self.handle_request(request, &tx).await?,

                    // No more requests from the `FilteringService`.
                    // HTTP connection is closed and possibly upgraded.
                    None => {
                        STEAL_FILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        break
                    }
                }
            }
        }

        Ok(queued_raw_data)
    }

    /// Runs this task after the HTTP connection was closed or upgraded.
    async fn run_after_http_ends(
        &mut self,
        mut queued_raw_data: HashMap<ClientId, Vec<Vec<u8>>>,
        tx: Sender<ConnectionMessageOut>,
        rx: &mut Receiver<ConnectionMessageIn>,
    ) -> Result<(), ConnectionTaskError> {
        let (task_handle, _drop_guard) = self
            .hyper_conn_task
            .take()
            .expect("task handle is consumed only here");

        let Some(upgraded) = task_handle
            .await
            .map_err(|_| ConnectionTaskError::HttpConnectionTaskPanicked)?
        else {
            return Ok(());
        };

        let parts = upgraded
            .http_client_io
            .downcast::<TokioIo<T>>()
            .expect("IO type is known");
        let mut http_client_io = parts.io.into_inner();
        let http_client_read_buf = parts.read_buf;

        match upgraded.http_server_io {
            // Connection was upgraded and some HTTP filter matched the upgrade request.
            UpgradedServerSide::MatchedClient(client_id) => {
                tracing::trace!(
                    connection_id = self.connection_id,
                    client_id,
                    "HTTP connection upgraded for client",
                );

                for data in queued_raw_data.remove(&client_id).unwrap_or_default() {
                    if data.is_empty() {
                        http_client_io.shutdown().await?;
                    } else {
                        http_client_io.write_all(&data).await?;
                    }
                }

                for (id, subscribed) in self.subscribed.iter_mut() {
                    if *subscribed && *id != client_id {
                        tx.send(ConnectionMessageOut::Closed {
                            client_id: *id,
                            connection_id: self.connection_id,
                        })
                        .await?;

                        *subscribed = false;
                    }
                }

                if self.subscribed.remove(&client_id) != Some(true) {
                    return Ok(());
                }

                if !http_client_read_buf.is_empty() {
                    tx.send(ConnectionMessageOut::Raw {
                        client_id,
                        connection_id: self.connection_id,
                        data: http_client_read_buf.into(),
                    })
                    .await?;
                }

                UnfilteredStealTask {
                    connection_id: self.connection_id,
                    client_id,
                    stream: http_client_io,
                }
                .run(tx, rx)
                .await
            }

            // Connection was upgraded and no HTTP filter matched the upgrade request.
            UpgradedServerSide::OriginalDestination(upgraded) => {
                tracing::trace!(
                    connection_id = self.connection_id,
                    "HTTP connection upgraded for original destination",
                );

                for (client_id, subscribed) in self.subscribed.iter_mut() {
                    if *subscribed {
                        tx.send(ConnectionMessageOut::Closed {
                            connection_id: self.connection_id,
                            client_id: *client_id,
                        })
                        .await?;

                        *subscribed = false;
                    }
                }

                let parts = upgraded
                    .downcast::<TokioIo<MaybeTls>>()
                    .expect("IO type is known");
                let mut http_server_io = parts.io.into_inner();
                let http_server_read_buf = parts.read_buf;

                tokio::try_join!(
                    async {
                        if http_client_read_buf.is_empty() {
                            return Ok(());
                        }

                        http_server_io.write_all(&http_client_read_buf).await
                    },
                    async {
                        if http_server_read_buf.is_empty() {
                            return Ok(());
                        }

                        http_client_io.write_all(&http_server_read_buf).await
                    },
                )?;

                tokio::io::copy_bidirectional(&mut http_client_io, &mut http_server_io).await?;

                Ok(())
            }
        }
    }

    /// Runs this task until the connection is closed.
    pub async fn run(
        mut self,
        tx: Sender<ConnectionMessageOut>,
        rx: &mut Receiver<ConnectionMessageIn>,
    ) -> Result<(), ConnectionTaskError> {
        let res = self.run_until_http_ends(tx.clone(), rx).await;

        STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics_updated = true;

        let res = match res {
            Ok(data) => self.run_after_http_ends(data, tx.clone(), rx).await,
            Err(e) => Err(e),
        };

        for (client_id, subscribed) in self.subscribed.iter() {
            if *subscribed {
                tx.send(ConnectionMessageOut::Closed {
                    client_id: *client_id,
                    connection_id: self.connection_id,
                })
                .await?;
            }
        }

        res
    }
}

#[cfg(test)]
mod test {

    use std::{fs, sync::Arc};

    use bytes::BytesMut;
    use http::{
        header::{CONNECTION, UPGRADE},
        HeaderValue, Method,
    };
    use http_body_util::Empty;
    use hyper::{client::conn::http1::SendRequest, service::service_fn};
    use mirrord_agent_env::steal_tls::{
        AgentClientConfig, AgentServerConfig, StealPortTlsConfig, TlsAuthentication,
        TlsClientVerification, TlsServerVerification,
    };
    use mirrord_protocol::LogLevel;
    use rustls::{
        crypto::CryptoProvider, pki_types::ServerName, server::WebPkiClientVerifier, ClientConfig,
        RootCertStore, ServerConfig,
    };
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
        task::JoinSet,
    };
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use super::*;
    use crate::{
        steal::{http::HttpFilter, tls, StealTlsHandlerStore},
        util::path_resolver::InTargetPathResolver,
    };

    /// Full setup for [`FilteredStealTask`] tests.
    struct TestSetup {
        /// [`HttpFilter`]s mapping used by the task.
        filters: Filters,
        /// Address of the original HTTP server (the one we steal from).
        original_address: SocketAddr,
        /// Stolen connection wrapped into HTTP.
        request_sender: SendRequest<DynamicBody>,
        /// For sending [`ConnectionMessageIn`] to the task.
        task_in_tx: Sender<ConnectionMessageIn>,
        /// For receiving [`ConnectionMessageOut`] from the task.
        task_out_rx: Receiver<ConnectionMessageOut>,
        /// Background tasks spawned during this test.
        tasks: JoinSet<()>,
        /// For cancelling the background task running the original HTTP server.
        original_server_token: CancellationToken,
    }

    impl TestSetup {
        /// Name of the dummy protocol we're upgrading to.
        /// Protocol goes like this:
        /// 1. The server sends bytes 'hello'.
        /// 2. The server echoes back everything it receives from the client.
        const TEST_PROTO: &'static str = "testproto";

        /// Id of the stolen connection.
        const CONNECTION_ID: ConnectionId = 0;

        /// Request handler of the original HTTP server.
        /// Responds with [`StatusCode::BAD_REQUEST`] to anything that is not a valid
        /// [`Self::TEST_PROTO`] upgrade request. Allows the connection to
        /// [`Self::TEST_PROTO`].
        async fn handle_request(
            request: Request<Incoming>,
        ) -> hyper::Result<Response<DynamicBody>> {
            let mut response = Response::new(Empty::new().map_err(|_| unreachable!()).boxed());
            *response.version_mut() = request.version();

            let contains_expected_upgrade = request
                .headers()
                .get(UPGRADE)
                .filter(|proto| *proto == Self::TEST_PROTO)
                .is_some()
                && request
                    .headers()
                    .get(CONNECTION)
                    .filter(|value| *value == "upgrade")
                    .is_some();

            if !contains_expected_upgrade {
                *response.status_mut() = StatusCode::BAD_REQUEST;
                return Ok(response);
            }

            task::spawn(async move {
                let upgraded = hyper::upgrade::on(request).await.unwrap();
                let parts = upgraded.downcast::<TokioIo<TcpStream>>().unwrap();
                let mut stream = parts.io.into_inner();

                stream.write_all(b"hello").await.unwrap();
                if !parts.read_buf.is_empty() {
                    stream.write_all(&parts.read_buf).await.unwrap();
                }
                stream.flush().await.unwrap();

                let mut buffer = BytesMut::with_capacity(4096);

                loop {
                    stream.read_buf(&mut buffer).await.unwrap();
                    if buffer.is_empty() {
                        break;
                    }
                    stream.write_all(&buffer).await.unwrap();
                    stream.flush().await.unwrap();
                    buffer.clear();
                }
            });

            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            response
                .headers_mut()
                .insert(UPGRADE, HeaderValue::from_static(Self::TEST_PROTO));
            response
                .headers_mut()
                .insert(CONNECTION, HeaderValue::from_static("upgrade"));

            Ok(response)
        }

        /// Creates a new instance of this struct.
        /// Spawns an HTTP server using [`Self::handle_request`], spawns [`FilteredStealTask`],
        /// prepares connections.
        async fn new() -> Self {
            let mut tasks = JoinSet::new();

            let original_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let original_address = original_listener.local_addr().unwrap();
            let original_server_token = CancellationToken::new();
            let token_clone = original_server_token.clone();

            let (server_stream, peer_address, client_stream) = {
                let stealing_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let ((server_stream, peer_address), client_stream) = tokio::try_join!(
                    stealing_listener.accept(),
                    TcpStream::connect(stealing_listener.local_addr().unwrap()),
                )
                .unwrap();

                (server_stream, peer_address, client_stream)
            };

            tasks.spawn(async move {
                let mut tasks = JoinSet::new();

                loop {
                    tokio::select! {
                        accept = original_listener.accept() => {
                            let (stream, _) = accept.unwrap();
                            let conn = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service_fn(Self::handle_request)).with_upgrades();

                            tasks.spawn(async move {
                                conn
                                    .await
                                    .unwrap();
                            });
                        },

                        Some(res) = tasks.join_next() => res.unwrap(),

                        _ = token_clone.cancelled() => break,
                    }
                }

                tasks.shutdown().await;
            });

            let filters: Filters = Default::default();
            let filters_clone = filters.clone();

            let (in_tx, mut in_rx) = mpsc::channel(8);
            let (out_tx, out_rx) = mpsc::channel(8);

            tasks.spawn(async move {
                let task = FilteredStealTask::new(
                    Self::CONNECTION_ID,
                    filters_clone,
                    OriginalDestination::new(original_address, None),
                    peer_address,
                    HttpVersion::V1,
                    server_stream,
                );

                task.run(out_tx, &mut in_rx).await.unwrap();
            });

            let (request_sender, conn) = hyper::client::conn::http1::handshake::<_, DynamicBody>(
                TokioIo::new(client_stream),
            )
            .await
            .unwrap();

            tasks.spawn(async move {
                conn.with_upgrades().await.unwrap();
            });

            TestSetup {
                filters,
                original_address,
                request_sender,
                task_in_tx: in_tx,
                task_out_rx: out_rx,
                tasks,
                original_server_token,
            }
        }

        /// Prepares a new [`Request`] to be sent via [`Self::request_sender`].
        ///
        /// # Params
        ///
        /// 1. `client_id` - If this is set to [`Some`] value, the request will contain a header
        ///    such that the [`FilteredStealTask`] will steal the request for the client.
        /// 2. `is_upgrade` - If `false`, an ordinary `GET` request will be produced. If `true`, a
        ///    [`Self::TEST_PROTO`] upgrade request will be produced.
        fn prepare_request(
            &self,
            client_id: Option<ClientId>,
            is_upgrade: bool,
        ) -> Request<DynamicBody> {
            let mut builder = Request::builder().method(Method::GET).uri(if is_upgrade {
                "testproto://www.some-server.com"
            } else {
                "http://www.some-server.com"
            });

            if let Some(client_id) = client_id {
                builder = builder.header("x-client", &client_id.to_string());
                self.filters.insert(
                    client_id,
                    (
                        HttpFilter::Header(format!("x-client: {client_id}").parse().unwrap()),
                        Some("1.19.0".parse().unwrap()),
                    ),
                );
            }

            if is_upgrade {
                builder = builder
                    .header(UPGRADE, Self::TEST_PROTO)
                    .header(CONNECTION, "upgrade");
            }

            builder
                .body(Empty::new().map_err(|_| unreachable!()).boxed())
                .unwrap()
        }

        /// Prepares a new [`Request`] to be sent via [`Self::request_sender`]. Inserts the same
        /// header filter for multiple clients.
        ///
        /// # Params
        ///
        /// 1. `client_ids` - For each client_id, insert a header filter. The request will contain a
        ///    corresponding header such that the [`FilteredStealTask`] will steal it..
        fn prepare_request_for_multiple(&self, client_ids: &[ClientId]) -> Request<DynamicBody> {
            let filter = (
                HttpFilter::Header("x-subscription: ABCD".parse().unwrap()),
                Some("1.19.0".parse().unwrap()),
            );

            for client_id in client_ids {
                self.filters.insert(*client_id, filter.clone());
            }

            Request::builder()
                .method(Method::GET)
                .uri("http://www.some-server.com")
                .header("x-subscription", "ABCD".to_string())
                .body(Empty::new().map_err(|_| unreachable!()).boxed())
                .unwrap()
        }

        /// 1. Closes the stolen connection and the original HTTP server.
        /// 2. Waits until [`FilteredStealTask`] finishes.
        /// 3. Checks results of the background tasks.
        async fn shutdown(mut self) -> Receiver<ConnectionMessageOut> {
            std::mem::drop(self.request_sender);
            self.task_in_tx.closed().await;
            self.original_server_token.cancel();

            while let Some(task_result) = self.tasks.join_next().await {
                task_result.unwrap();
            }

            self.task_out_rx
        }
    }

    /// Stolen connection receives 5 requests.
    /// The first one does not match any client's filter.
    /// Each consecutive request matches some other client's filter.
    /// Then, connection is closed and all 4 clients are notified.
    #[tokio::test]
    async fn multiple_clients() {
        let mut setup = TestSetup::new().await;

        // First request does not match any filter, should reach the original destination.
        let request = setup.prepare_request(None, false);
        let response = setup.request_sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Consecutive requests should reach stealer clients.
        for client_id in 0..4 {
            let request = setup.prepare_request(Some(client_id), false);
            tokio::join!(
                async {
                    let response = setup.request_sender.send_request(request).await.unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                },
                async {
                    match setup.task_out_rx.recv().await.unwrap() {
                        ConnectionMessageOut::SubscribedHttp {
                            client_id: received_client_id,
                            connection_id: TestSetup::CONNECTION_ID,
                        } => {
                            assert_eq!(received_client_id, client_id);
                        }
                        other => unreachable!("unexpected message: {other:?}"),
                    };

                    let request_id = match setup.task_out_rx.recv().await.unwrap() {
                        ConnectionMessageOut::Request {
                            client_id: received_client_id,
                            connection_id: TestSetup::CONNECTION_ID,
                            id,
                            metadata: HttpRequestMetadata::V1 { destination, .. },
                            transport: HttpRequestTransportType::Tcp,
                            ..
                        } => {
                            assert_eq!(received_client_id, client_id);
                            assert_eq!(destination, setup.original_address);
                            id
                        }
                        other => unreachable!("unexpected message: {other:?}"),
                    };

                    let response = Response::builder()
                        .status(StatusCode::OK)
                        .body(Empty::new().map_err(|_| unreachable!()).boxed())
                        .unwrap();

                    setup
                        .task_in_tx
                        .send(ConnectionMessageIn::Response {
                            client_id,
                            request_id,
                            response,
                        })
                        .await
                        .unwrap();
                }
            );
        }

        let mut rx = tokio::time::timeout(std::time::Duration::from_secs(5), setup.shutdown())
            .await
            .unwrap();

        let mut clients_closed = [false; 4];
        for _ in 0..4 {
            match rx.recv().await.unwrap() {
                ConnectionMessageOut::Closed {
                    client_id,
                    connection_id: TestSetup::CONNECTION_ID,
                } => {
                    *clients_closed
                        .get_mut(usize::try_from(client_id).unwrap())
                        .expect("unexpected client id") = true;
                }
                other => unreachable!("unexpected message: {other:?}"),
            };
        }

        assert!(clients_closed.iter().all(|closed| *closed));

        assert!(rx.recv().await.is_none());
    }

    /// Stolen connection receives a request that matches 3 filters. The first client receives the
    /// request, the remanining two receive warnings.
    #[tokio::test]
    async fn multiple_clients_with_same_filter() {
        let mut setup = TestSetup::new().await;

        let request = setup.prepare_request_for_multiple(&[0, 1, 2]);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            },
            async {
                // One of the clients is subscribed and receives the request
                let subscribed_client_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::SubscribedHttp {
                        client_id: subscribed_client_id,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => subscribed_client_id,
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: received_client_id,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(received_client_id, subscribed_client_id);
                        assert_eq!(destination, setup.original_address);
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                // The remaining two cleints receive the warning
                for _ in 0..2 {
                    match setup.task_out_rx.recv().await.unwrap() {
                        ConnectionMessageOut::LogMessage {
                            client_id: received_client_id,
                            connection_id: TestSetup::CONNECTION_ID,
                            message:
                                LogMessage {
                                    message: _,
                                    level: LogLevel::Warn,
                                },
                        } => {
                            assert_ne!(received_client_id, subscribed_client_id);
                        }
                        other => unreachable!("unexpected message: {other:?}"),
                    }
                }

                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Empty::new().map_err(|_| unreachable!()).boxed())
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Response {
                        client_id: subscribed_client_id,
                        request_id,
                        response,
                    })
                    .await
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Unsubscribed {
                        client_id: subscribed_client_id,
                    })
                    .await
                    .unwrap();
            }
        );

        let mut rx = setup.shutdown().await;
        // The task should not produce the `Closed` message - the client has unsubscribed.
        assert!(rx.recv().await.is_none());
    }

    /// Stolen connection receives 2 requests, both match the same client's filter.
    /// After processing 2 requests, client unsubscribes the connection.
    /// Then, connection is closed and the client is not notified.
    #[tokio::test]
    async fn subscribed_unsubscribed() {
        let mut setup = TestSetup::new().await;

        // The client should see the `SubscribedHttp` message before the first request.
        let request = setup.prepare_request(Some(0), false);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            },
            async {
                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::SubscribedHttp {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => {}
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(destination, setup.original_address);
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Empty::new().map_err(|_| unreachable!()).boxed())
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Response {
                        client_id: 0,
                        request_id,
                        response,
                    })
                    .await
                    .unwrap();
            }
        );

        // The client should not see the `SusbcribedHttp` message before the second request.
        let request = setup.prepare_request(Some(0), false);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            },
            async {
                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(destination, setup.original_address);
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Empty::new().map_err(|_| unreachable!()).boxed())
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Response {
                        client_id: 0,
                        request_id,
                        response,
                    })
                    .await
                    .unwrap();
            }
        );

        setup
            .task_in_tx
            .send(ConnectionMessageIn::Unsubscribed { client_id: 0 })
            .await
            .unwrap();

        let mut rx = setup.shutdown().await;
        // The task should not produce the `Closed` message - the client has unsubscribed.
        assert!(rx.recv().await.is_none());
    }

    /// Stolen connection receives a request that matches some client filter.
    /// Then, the connection receives an upgrade request that does not match any filter.
    /// The client is notified that the connection has closed and bytes are proxied between the
    /// original HTTP server and the request sender.
    #[tokio::test]
    async fn upgrade_for_original_dst() {
        let mut setup = TestSetup::new().await;

        let request = setup.prepare_request(Some(0), false);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            },
            async {
                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::SubscribedHttp {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => {}
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(destination, setup.original_address);
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Empty::new().map_err(|_| unreachable!()).boxed())
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Response {
                        client_id: 0,
                        request_id,
                        response,
                    })
                    .await
                    .unwrap();
            }
        );

        let request = setup.prepare_request(None, true);
        let response = setup.request_sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let upgraded = hyper::upgrade::on(response).await.unwrap();
        let mut stream = TokioIo::new(upgraded);

        let mut buffer = *b"hello can you hear me";
        stream
            .write_all(buffer.strip_prefix(b"hello").unwrap())
            .await
            .unwrap();
        stream.flush().await.unwrap();
        stream.read_exact(&mut buffer).await.unwrap();

        assert_eq!(&buffer, b"hello can you hear me");

        std::mem::drop(stream);
        let mut rx = setup.shutdown().await;

        match rx.recv().await.unwrap() {
            ConnectionMessageOut::Closed {
                client_id: 0,
                connection_id: TestSetup::CONNECTION_ID,
            } => {}
            other => unreachable!("unexpected message: {other:?}"),
        }

        assert!(rx.recv().await.is_none());
    }

    /// Stolen connection receives a request that matches some client's filter.
    /// Then, the connection receives an upgrade request that matches some other client's filter.
    /// The first client is notified that the connection has closed and bytes are proxied between
    /// the second client and the request sender.
    #[tokio::test]
    async fn upgrade_for_stealer_client() {
        let mut setup = TestSetup::new().await;

        let request = setup.prepare_request(Some(0), false);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            },
            async {
                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::SubscribedHttp {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => {}
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(destination, setup.original_address);
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Empty::new().map_err(|_| unreachable!()).boxed())
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Response {
                        client_id: 0,
                        request_id,
                        response,
                    })
                    .await
                    .unwrap();
            }
        );

        let request = setup.prepare_request(Some(1), true);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
                let mut upgraded = TokioIo::new(hyper::upgrade::on(response).await.unwrap());
                upgraded.write_all(b"hello from client").await.unwrap();
                let mut buffer = *b"hello from server";
                upgraded.read_exact(&mut buffer).await.unwrap();
                assert_eq!(&buffer, b"hello from server");
            },
            async {
                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::SubscribedHttp {
                        client_id: 1,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => {}
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 1,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        request,
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(destination, setup.original_address);
                        assert_eq!(
                            request.headers().get(UPGRADE).and_then(|v| v.to_str().ok()),
                            Some(TestSetup::TEST_PROTO)
                        );
                        assert_eq!(
                            request
                                .headers()
                                .get(CONNECTION)
                                .and_then(|v| v.to_str().ok()),
                            Some("upgrade")
                        );
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                let response = Response::builder()
                    .status(StatusCode::SWITCHING_PROTOCOLS)
                    .header(UPGRADE, TestSetup::TEST_PROTO)
                    .header(CONNECTION, "upgrade")
                    .body(Empty::new().map_err(|_| unreachable!()).boxed())
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Response {
                        client_id: 1,
                        request_id,
                        response,
                    })
                    .await
                    .unwrap();

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Raw {
                        client_id: 1,
                        data: b"hello from server".to_vec(),
                    })
                    .await
                    .unwrap();

                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Closed {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => {}
                    other => unreachable!("unexpected message: {other:?}"),
                }

                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Raw {
                        client_id: 1,
                        connection_id: TestSetup::CONNECTION_ID,
                        data,
                    } => {
                        assert_eq!(&data, b"hello from client");
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                }

                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Raw {
                        client_id: 1,
                        connection_id: TestSetup::CONNECTION_ID,
                        data,
                    } => {
                        assert!(data.is_empty());
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                }
            }
        );

        setup
            .task_in_tx
            .send(ConnectionMessageIn::Unsubscribed { client_id: 1 })
            .await
            .unwrap();

        let mut rx = setup.shutdown().await;
        let next_message = rx.recv().await;
        assert!(next_message.is_none(), "unexpected: {next_message:?}");
    }

    /// The stolen connection receives a request that does not match any filter.
    /// The original HTTP server is dead and the request sender gets a [`StatusCode::BAD_GATEWAY`]
    /// response.
    #[tokio::test]
    async fn bad_response_original_server() {
        let mut setup = TestSetup::new().await;

        // Killing the original server.
        setup.original_server_token.cancel();
        setup.tasks.join_next().await.unwrap().unwrap();

        let request = setup.prepare_request(None, false);
        let response = setup.request_sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    /// The stolen connection receives a request that matches some client's filter.
    /// The client unsubscribes before providing a response and the request sender gets a
    /// [`StatusCode::BAD_GATEWAY`] response.
    #[tokio::test]
    async fn client_unsubscribed_while_blocking_a_request() {
        let mut setup = TestSetup::new().await;

        let request = setup.prepare_request(Some(0), false);
        tokio::join!(
            async {
                let response = setup.request_sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            },
            async {
                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::SubscribedHttp {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                    } => {}
                    other => unreachable!("unexpected message: {other:?}"),
                };

                match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        metadata: HttpRequestMetadata::V1 { destination, .. },
                        transport: HttpRequestTransportType::Tcp,
                        ..
                    } => {
                        assert_eq!(destination, setup.original_address);
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::Unsubscribed { client_id: 0 })
                    .await
                    .unwrap();
            }
        );

        let mut rx = setup.shutdown().await;
        // The task should not produce the `Closed` message - the client has unsubscribed.
        assert!(rx.recv().await.is_none());
    }

    /// Verifies that [`FilteredStealTask`] correctly handles HTTPS connections.
    ///
    /// The setup here uses:
    /// 1. An HTTPS server that expects ALPN upgrade to `h2` and always responds with
    ///    [`StatusCode::OK`].
    /// 2. An HTTPS client that uses ALPN upgrade to `h2` and sends two requests. The first request
    ///    is expected to result in [`StatusCode::IM_A_TEAPOT`] and contains a header that matches a
    ///    filter. The second request is expected to result in [`StatusCode::OK`] and does not
    ///    contain the header.
    ///
    /// All parties are configured with strict TLS verification.
    #[tokio::test]
    async fn with_tls() {
        /// Request handler for the original HTTPS server.
        async fn handle_request(
            mut request: hyper::Request<Incoming>,
        ) -> hyper::Result<Response<DynamicBody>> {
            while let Some(frame) = request.body_mut().frame().await {
                frame?;
            }

            let mut response = Response::new(Empty::new().map_err(|_| unreachable!()).boxed());
            *response.version_mut() = request.version();
            *response.status_mut() = StatusCode::OK;

            Ok(response)
        }

        let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

        let original_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let original_destination = original_listener.local_addr().unwrap();

        // Root certificate trusted by everyone.
        let common_root = mirrord_tls_util::generate_cert("root", None, true).unwrap();
        let original_server_chain = tls::test::CertChainWithKey::new("server", Some(&common_root));
        let agent_server_chain = tls::test::CertChainWithKey::new("server", Some(&common_root));
        let client_chain = tls::test::CertChainWithKey::new("client", Some(&common_root));
        let root_store = {
            let mut store = RootCertStore::empty();
            store.add(common_root.cert.der().clone()).unwrap();
            Arc::new(store)
        };

        let handler = {
            let root_dir = tempfile::tempdir().unwrap();
            let root_pem = root_dir.path().join("root.pem");
            fs::write(root_pem, common_root.cert.pem()).unwrap();
            let auth_pem = root_dir.path().join("auth.pem");
            agent_server_chain.to_file(&auth_pem);

            let config = StealPortTlsConfig {
                port: original_destination.port(),
                agent_as_server: AgentServerConfig {
                    authentication: TlsAuthentication {
                        cert_pem: "auth.pem".into(),
                        key_pem: "auth.pem".into(),
                    },
                    verification: Some(TlsClientVerification {
                        allow_anonymous: false,
                        accept_any_cert: false,
                        trust_roots: vec!["/root.pem".into()],
                    }),
                    alpn_protocols: vec![std::str::from_utf8(tls::HTTP_2_ALPN_NAME)
                        .unwrap()
                        .into()],
                },
                agent_as_client: AgentClientConfig {
                    authentication: Some(TlsAuthentication {
                        cert_pem: "auth.pem".into(),
                        key_pem: "auth.pem".into(),
                    }),
                    verification: TlsServerVerification {
                        accept_any_cert: false,
                        trust_roots: vec!["/root.pem".into()],
                    },
                },
            };
            let store = StealTlsHandlerStore::new(
                vec![config],
                InTargetPathResolver::with_root_path(root_dir.path().to_path_buf()),
            );
            store
                .get(original_destination.port())
                .await
                .unwrap()
                .unwrap()
        };

        let _original_server_task = tokio::spawn({
            let verifier = WebPkiClientVerifier::builder(root_store.clone())
                .build()
                .unwrap();
            let mut server_config = ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(original_server_chain.certs, original_server_chain.key)
                .unwrap();
            server_config
                .alpn_protocols
                .push(tls::HTTP_2_ALPN_NAME.into());
            let acceptor = TlsAcceptor::from(Arc::new(server_config));

            async move {
                loop {
                    let stream = original_listener.accept().await.unwrap().0;
                    let acceptor = acceptor.clone();

                    tokio::spawn(async move {
                        let stream = acceptor.accept(stream).await.unwrap();

                        match stream.get_ref().1.alpn_protocol() {
                            Some(tls::HTTP_2_ALPN_NAME) => {
                                hyper::server::conn::http2::Builder::new(TokioExecutor::default())
                                    .serve_connection(
                                        TokioIo::new(stream),
                                        service_fn(handle_request),
                                    )
                                    .await
                                    .unwrap();
                            }
                            other => panic!("unexpected ALPN upgrade: {other:?}"),
                        }
                    });
                }
            }
        });

        let agent_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent_addr = agent_listener.local_addr().unwrap();

        let client_task = tokio::spawn({
            let mut client_config = ClientConfig::builder()
                .with_root_certificates(root_store.clone())
                .with_client_auth_cert(client_chain.certs, client_chain.key)
                .unwrap();
            client_config
                .alpn_protocols
                .extend([tls::HTTP_1_1_ALPN_NAME.into(), tls::HTTP_2_ALPN_NAME.into()]);
            let connector = TlsConnector::from(Arc::new(client_config));

            async move {
                let stream = TcpStream::connect(agent_addr).await.unwrap();
                let stream = connector
                    .connect(ServerName::try_from("server").unwrap(), stream)
                    .await
                    .unwrap();
                assert_eq!(
                    stream.get_ref().1.alpn_protocol(),
                    Some(tls::HTTP_2_ALPN_NAME)
                );
                let (mut sender, conn) = hyper::client::conn::http2::handshake(
                    TokioExecutor::new(),
                    TokioIo::new(stream),
                )
                .await
                .unwrap();
                tokio::spawn(conn);

                let mut request = Request::new(Empty::<Bytes>::new());
                request
                    .headers_mut()
                    .insert("x-capture", HeaderValue::from_static("true"));
                let mut response = sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::IM_A_TEAPOT); // request handled by client.
                let _ = response.body_mut().collect().await.unwrap();

                let request = Request::new(Empty::<Bytes>::new());
                let mut response = sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK); // request handled by by the original server.
                let _ = response.body_mut().collect().await.unwrap();
            }
        });

        let filters = Filters::default();
        filters.insert(
            0,
            (
                HttpFilter::Header("x-capture: true".parse().unwrap()),
                Some("1.19.0".parse().unwrap()),
            ),
        );

        let (stream, peer_address) = agent_listener.accept().await.unwrap();
        let stream = handler.acceptor().accept(stream).await.unwrap();
        assert_eq!(
            stream.get_ref().1.alpn_protocol(),
            Some(tls::HTTP_2_ALPN_NAME)
        );
        assert_eq!(stream.get_ref().1.server_name(), Some("server"));
        let original_destination = OriginalDestination::new(
            original_destination,
            Some(handler.connector(stream.get_ref().1)),
        );

        let filtered_steal_task = FilteredStealTask::new(
            0,
            filters,
            original_destination,
            peer_address,
            HttpVersion::V2,
            Box::new(stream),
        );

        let (msg_in_tx, mut msg_in_rx) = mpsc::channel(8);
        let (msg_out_tx, mut msg_out_rx) = mpsc::channel(8);
        let task_handle = tokio::spawn(async move {
            filtered_steal_task
                .run(msg_out_tx, &mut msg_in_rx)
                .await
                .unwrap();
        });

        match msg_out_rx.recv().await.unwrap() {
            ConnectionMessageOut::SubscribedHttp {
                client_id: 0,
                connection_id: 0,
            } => {}
            other => panic!("unexpected message from filtered steal task: {other:?}"),
        }

        match msg_out_rx.recv().await.unwrap() {
            ConnectionMessageOut::Request {
                client_id: 0,
                connection_id: 0,
                id: 0,
                transport:
                    HttpRequestTransportType::Tls {
                        alpn_protocol,
                        server_name,
                    },
                ..
            } => {
                assert_eq!(alpn_protocol, Some(tls::HTTP_2_ALPN_NAME.into()));
                assert_eq!(server_name, Some("server".into()));
            }
            other => panic!("unexpected message from filtered steal task: {other:?}"),
        }

        let mut response = Response::new(Empty::<Bytes>::new().map_err(|_| unreachable!()).boxed());
        *response.status_mut() = StatusCode::IM_A_TEAPOT;
        msg_in_tx
            .send(ConnectionMessageIn::Response {
                client_id: 0,
                request_id: 0,
                response,
            })
            .await
            .unwrap();

        match msg_out_rx.recv().await.unwrap() {
            ConnectionMessageOut::Closed {
                client_id: 0,
                connection_id: 0,
            } => {}
            other => panic!("unexpected message from filtered steal task: {other:?}"),
        }

        // assert that the client received expected messages from both the client and the original
        // server
        client_task.await.unwrap();
        // assert that the filtered steal task did not fail
        task_handle.await.unwrap();
    }
}
