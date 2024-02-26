use std::{
    collections::HashMap,
    future::{poll_fn, Future},
    marker::PhantomData,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
};

use bytes::Bytes;
use dashmap::DashMap;
use http::Version;
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
use mirrord_protocol::{ConnectionId, RequestId};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    task::{self, JoinHandle},
};
use tokio_util::sync::{CancellationToken, DropGuard};

use super::{ConnectionMessageIn, ConnectionMessageOut, ConnectionTaskError};
use crate::{
    http::HttpVersion,
    steal::{connections::unfiltered::UnfilteredStealTask, http::HttpFilter},
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
    /// The [`Request`] should be handled by the HTTP server running at the given address.
    LetThrough {
        to: SocketAddr,
        unchanged: Request<Incoming>,
    },
    /// The [`FilteringService`] should respond immediately with the given [`Response`]
    /// on behalf of the given stealer client.
    RespondWith {
        response: Response<DynamicBody>,
        for_client: ClientId,
    },
}

/// HTTP server side of an upgraded connection retrieved from [`FilteringService`].
pub enum UpgradedServerSide {
    /// Stealer client. Their [`HttpFilter`] matched the upgrade request.
    /// The rest of the connection should be proxied between the HTTP client and this stealer
    /// client (which acts as an HTTP server).
    MatchedClient(ClientId),
    /// TCP connection with the HTTP server that was the original destination of the HTTP client
    /// (no [`HttpFilter`] matched the upgrade request). The rest of the connection should be
    /// proxied between the HTTP client and this HTTP server.
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
    /// possible). However, using a [`oneshot`] here would require a combination of an [`Arc`],
    /// a [`Mutex`](std::sync::Mutex) and an [`Option`]. [`mpsc`] is used here for simplicity.
    upgrade_tx: Sender<UpgradedConnection>,
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

    /// Sends the given [`Request`] to the destination given as `to`.
    ///
    /// # TODO
    ///
    /// This method always creates a new TCP connection and preforms an HTTP handshake.
    /// Also, it does not retry the request upon failure.
    async fn send_request(
        to: SocketAddr,
        request: Request<Incoming>,
    ) -> Option<Response<Incoming>> {
        let tcp_stream = TcpStream::connect(to)
            .await
            .inspect_err(|error| {
                tracing::error!(?error, "Failed connecting to request destination");
            })
            .ok()?;

        match request.version() {
            Version::HTTP_2 => {
                let (mut request_sender, connection) =
                    http2::handshake(TokioExecutor::default(), TokioIo::new(tcp_stream))
                        .await
                        .inspect_err(|error| {
                            tracing::error!(
                                ?error,
                                "HTTP2 handshake with original destination failed"
                            )
                        })
                        .ok()?;

                // We need this to progress the connection forward (hyper thing).
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::error!(?error, "Connection with original destination failed");
                    }
                });

                request_sender.send_request(request).await.ok()
            }

            _ => {
                let (mut request_sender, mut connection) =
                    http1::handshake(TokioIo::new(tcp_stream))
                        .await
                        .inspect_err(|error| {
                            tracing::error!(
                                ?error,
                                "HTTP1 handshake with original destination failed"
                            )
                        })
                        .ok()?;

                // We need this to progress the connection forward (hyper thing).
                tokio::spawn(async move {
                    if let Err(error) = poll_fn(|cx| connection.poll_without_shutdown(cx)).await {
                        tracing::error!(?error, "Connection with original destination failed");
                    }
                });

                request_sender.send_request(request).await.ok()
            }
        }
    }

    /// Sends the given [`Request`] to the destination given as `to`. If the [`Response`] is
    /// [`StatusCode::SWITCHING_PROTOCOLS`], spawns a new [`tokio::task`] to await for upgrade.
    async fn let_through(
        &self,
        to: SocketAddr,
        mut request: Request<Incoming>,
    ) -> Response<DynamicBody> {
        let http_client_on_upgrade = hyper::upgrade::on(&mut request);

        let version = request.version();
        let mut response = Self::send_request(to, request)
            .await
            .map(|response| response.map(BoxBody::new))
            .unwrap_or_else(|| {
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
                    tokio::try_join!(http_client_on_upgrade, http_server_on_upgrade)
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
            Ok(RequestHandling::LetThrough { to, unchanged }) => {
                self.let_through(to, unchanged).await
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
    /// Original destination of the stolen connection. Used when passing through HTTP requests that
    /// don't not match any filter in [`Self::filters`].
    original_destination: SocketAddr,

    /// Stealer client to [`HttpFilter`] mapping. Allows for routing HTTP requests to correct
    /// stealer clients.
    ///
    /// # Note
    ///
    /// This mapping is shared via [`Arc`], allowing for dynamic updates from the outside.
    /// This allows for *injecting* new stealer clients into exisiting connections.
    filters: Arc<DashMap<ClientId, HttpFilter>>,

    /// Stealer client to subscription state mapping.
    /// 1. `true` -> client is subscribed
    /// 2. `false` -> client has unsubscribed or we sent [`ConnectionMessageOut::Closed`].
    /// 3. `None` -> client does not know about this connection at all
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
}

impl<T> FilteredStealTask<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Creates a new instance of this task. The task will manage the connection given as `io` and
    /// use the provided `filters` for matching incoming [`Request`]s with stealing clients.
    ///
    /// The task will not run yet, see [`Self::run`].
    pub fn new(
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, HttpFilter>>,
        original_destination: SocketAddr,
        http_version: HttpVersion,
        io: T,
    ) -> Self {
        let (upgrade_tx, mut upgrade_rx) = mpsc::channel(1);
        let (requests_tx, requests_rx) = mpsc::channel(8);

        let service = FilteringService {
            requests_tx,
            upgrade_tx,
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

        Self {
            connection_id,
            original_destination,
            filters,
            subscribed: Default::default(),
            requests_rx,
            hyper_conn_task: Some((task_handle, drop_guard)),
            blocked_requests: Default::default(),
            next_request_id: Default::default(),
            _io_type: Default::default(),
        }
    }

    /// Matches the given [`Request`] against [`Self::filters`] and state of [`Self::subscribed`].
    fn match_request<B>(&self, request: &mut Request<B>) -> Option<ClientId> {
        self.filters
            .iter()
            .filter_map(|entry| entry.value().matches(request).then(|| *entry.key()))
            .find(|client_id| self.subscribed.get(client_id).copied().unwrap_or(true))
    }

    /// Sends the given [`Response`] to the [`FilteringService`] via [`oneshot::Sender`] from
    /// [`Self::blocked_requests`].
    ///
    /// If there is no blocked request for the given ([`ClientId`], [`RequestId`]) combination or
    /// the HTTP connection is dead, does nothing.
    fn handle_response(
        &mut self,
        client_id: ClientId,
        request_id: RequestId,
        response: Response<DynamicBody>,
    ) {
        let Some(tx) = self.blocked_requests.remove(&(client_id, request_id)) else {
            tracing::trace!(
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
            tracing::trace!(
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
    fn handle_response_failure(&mut self, client_id: ClientId, request_id: RequestId) {
        let removed = self
            .blocked_requests
            .remove(&(client_id, request_id))
            .is_some();
        if !removed {
            tracing::trace!(
                client_id,
                request_id,
                connection_id = self.connection_id,
                "Received a response failure for an unexpected (client_id, request_id) combination",
            );
        }
    }

    /// Handles a [`Request`] intercepted by the [`FilteringService`].
    async fn handle_request(
        &mut self,
        mut request: ExtractedRequest,
        tx: &Sender<ConnectionMessageOut>,
    ) -> Result<(), ConnectionTaskError> {
        let Some(client_id) = self.match_request(&mut request.request) else {
            let _ = request.response_tx.send(RequestHandling::LetThrough {
                to: self.original_destination,
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

        tx.send(ConnectionMessageOut::Request {
            client_id,
            connection_id: self.connection_id,
            request: request.request,
            id,
            port: self.original_destination.port(),
        })
        .await?;

        self.blocked_requests
            .insert((client_id, id), request.response_tx);

        Ok(())
    }

    /// Runs this task until the connection is closed.
    /// Handles HTTP upgrades by transforming into an [`UnfilteredStealTask`].
    ///
    /// # Note
    ///
    /// Does not send [`ConnectionMessageOut::Closed`], leaving this up to wrapper method
    /// [`Self::run`]. [`Self::run`] calls this method and then sends
    /// [`ConnectionMessageOut::Closed`] based on state in [`Self::subscribed`].
    async fn run_inner(
        &mut self,
        tx: Sender<ConnectionMessageOut>,
        rx: &mut Receiver<ConnectionMessageIn>,
    ) -> Result<(), ConnectionTaskError> {
        let mut queued_raw_data = vec![];

        loop {
            tokio::select! {
                message = rx.recv() => match message.ok_or(ConnectionTaskError::RecvError)? {
                    ConnectionMessageIn::Raw { data, client_id } => {
                        tracing::trace!(client_id, connection_id = self.connection_id, "Received raw data");
                        queued_raw_data.push((data, client_id));
                    },
                    ConnectionMessageIn::Response { response, request_id, client_id } => {
                        self.handle_response(client_id, request_id, response);
                        queued_raw_data.retain(|(_, id)| *id != client_id);
                    },
                    ConnectionMessageIn::ResponseFailed { request_id, client_id } => {
                        self.handle_response_failure(client_id, request_id);
                        queued_raw_data.retain(|(_, id)| *id != client_id);
                    },
                    ConnectionMessageIn::Unsubscribed { client_id } => {
                        self.subscribed.insert(client_id, false);
                        self.blocked_requests.retain(|key, _| key.0 != client_id);
                    },
                },

                request = self.requests_rx.recv() => match request {
                    Some(request) => self.handle_request(request, &tx).await?,

                    // No more requests from the `FilteringService`.
                    // HTTP connection is closed and possibly upgraded.
                    None => break,
                }
            }
        }

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

                for (data, _) in queued_raw_data
                    .into_iter()
                    .filter(|(_, id)| *id == client_id)
                {
                    http_client_io.write_all(&data).await?;
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

                if self.subscribed.get(&client_id).copied() != Some(true) {
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
                    .downcast::<TokioIo<TcpStream>>()
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
        let res = self.run_inner(tx.clone(), rx).await;

        for (client_id, subscribed) in self.subscribed {
            if subscribed {
                tx.send(ConnectionMessageOut::Closed {
                    client_id,
                    connection_id: self.connection_id,
                })
                .await?;
            }
        }

        res
    }
}
