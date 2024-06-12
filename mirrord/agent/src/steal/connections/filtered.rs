use std::{
    collections::HashMap, future::Future, marker::PhantomData, net::SocketAddr, pin::Pin, sync::Arc,
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
        mut request: Request<Incoming>,
    ) -> Result<Response<Incoming>, Box<dyn std::error::Error>> {
        let tcp_stream = TcpStream::connect(to).await.inspect_err(|error| {
            tracing::error!(?error, address = %to, "Failed connecting to request destination");
        })?;

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
                        })?;

                // We need this to progress the connection forward (hyper thing).
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::error!(?error, "Connection with original destination failed");
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
                let (mut request_sender, connection) = http1::handshake(TokioIo::new(tcp_stream))
                    .await
                    .inspect_err(|error| {
                        tracing::error!(?error, "HTTP1 handshake with original destination failed")
                    })?;

                // We need this to progress the connection forward (hyper thing).
                tokio::spawn(async move {
                    if let Err(error) = connection.with_upgrades().await {
                        tracing::error!(?error, "Connection with original destination failed");
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
        to: SocketAddr,
    ) -> Response<DynamicBody> {
        let version = request.version();
        let mut response = Self::send_request(to, request)
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
            Ok(RequestHandling::LetThrough { to, unchanged }) => {
                self.let_through(unchanged, on_upgrade, to).await
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
        filters: Arc<DashMap<ClientId, HttpFilter>>,
        original_destination: SocketAddr,
        http_version: HttpVersion,
        io: T,
    ) -> Self {
        let (upgrade_tx, mut upgrade_rx) = mpsc::channel(1);
        let (requests_tx, requests_rx) = mpsc::channel(Self::MAX_CONCURRENT_REQUESTS);

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
    #[tracing::instrument(
        level = "trace",
        name = "match_request_with_filter",
        skip(self, request),
        fields(
            request_path = request.uri().path(),
            request_headers = ?request.headers(),
            filters = ?self.filters,
        )
        ret,
    )]
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
    #[tracing::instrument(
        level = "trace",
        name = "handle_filtered_request_response",
        skip(self, response),
        fields(
            status = u16::from(response.status()),
            connection_id = self.connection_id,
            original_destination = %self.original_destination,
        )
    )]
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
    #[tracing::instrument(
        level = "trace",
        name = "handle_filtered_request_response_failure",
        skip(self),
        fields(
            connection_id = self.connection_id,
            original_destination = %self.original_destination,
        )
    )]
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
    #[tracing::instrument(level = "trace", skip(self, request, tx), ret, err(Debug))]
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

    /// Runs this task until the HTTP connection is closed or upgraded.
    ///
    /// # Returns
    ///
    /// Returns raw data sent by stealer clients after all their HTTP responses (to catch the case
    /// when the client starts sending data immediately after the `101 SWITCHING PROTOCOLS`).
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
                    ConnectionMessageIn::ResponseFailed { request_id, client_id } => {
                        queued_raw_data.remove(&client_id);
                        self.handle_response_failure(client_id, request_id);
                    },
                    ConnectionMessageIn::Unsubscribed { client_id } => {
                        queued_raw_data.remove(&client_id);
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
        let res = self.run_until_http_ends(tx.clone(), rx).await;

        let res = match res {
            Ok(data) => self.run_after_http_ends(data, tx.clone(), rx).await,
            Err(e) => Err(e),
        };

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

#[cfg(test)]
mod test {
    use bytes::BytesMut;
    use http::{
        header::{CONNECTION, UPGRADE},
        HeaderValue, Method,
    };
    use http_body_util::Empty;
    use hyper::{client::conn::http1::SendRequest, service::service_fn};
    use tokio::{io::AsyncReadExt, net::TcpListener, task::JoinSet};

    use super::*;

    /// Full setup for [`FilteredStealTask`] tests.
    struct TestSetup {
        /// [`HttpFilter`]s mapping used by the task.
        filters: Arc<DashMap<ClientId, HttpFilter>>,
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

            let (server_stream, client_stream) = {
                let stealing_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let ((server_stream, _), client_stream) = tokio::try_join!(
                    stealing_listener.accept(),
                    TcpStream::connect(stealing_listener.local_addr().unwrap()),
                )
                .unwrap();

                (server_stream, client_stream)
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

            let filters: Arc<DashMap<ClientId, HttpFilter>> = Default::default();
            let filters_clone = filters.clone();

            let (in_tx, mut in_rx) = mpsc::channel(8);
            let (out_tx, out_rx) = mpsc::channel(8);

            tasks.spawn(async move {
                let task = FilteredStealTask::new(
                    Self::CONNECTION_ID,
                    filters_clone,
                    original_address,
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
                    HttpFilter::Header(format!("x-client: {client_id}").parse().unwrap()),
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
                            port,
                            ..
                        } => {
                            assert_eq!(received_client_id, client_id);
                            assert_eq!(port, setup.original_address.port());
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
                    clients_closed[usize::try_from(client_id).unwrap()] = true;
                }
                other => unreachable!("unexpected message: {other:?}"),
            };
        }

        assert!(clients_closed.iter().all(|closed| *closed));

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
                        port,
                        ..
                    } => {
                        assert_eq!(port, setup.original_address.port());
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
                        port,
                        ..
                    } => {
                        assert_eq!(port, setup.original_address.port());
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
                        port,
                        ..
                    } => {
                        assert_eq!(port, setup.original_address.port());
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
                        port,
                        ..
                    } => {
                        assert_eq!(port, setup.original_address.port());
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
                        port,
                        request,
                    } => {
                        assert_eq!(port, setup.original_address.port());
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
    /// The client fails to provide a response and the request sender gets a
    /// [`StatusCode::BAD_GATEWAY`] response.
    #[tokio::test]
    async fn response_from_client_failed() {
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

                let request_id = match setup.task_out_rx.recv().await.unwrap() {
                    ConnectionMessageOut::Request {
                        client_id: 0,
                        connection_id: TestSetup::CONNECTION_ID,
                        id,
                        port,
                        ..
                    } => {
                        assert_eq!(port, setup.original_address.port());
                        id
                    }
                    other => unreachable!("unexpected message: {other:?}"),
                };

                setup
                    .task_in_tx
                    .send(ConnectionMessageIn::ResponseFailed {
                        client_id: 0,
                        request_id,
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
                        port,
                        ..
                    } => {
                        assert_eq!(port, setup.original_address.port());
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
}
