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
use http_body_util::{combinators::BoxBody, BodyExt, Empty};
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

struct ExtractedRequest {
    request: Request<Incoming>,
    response_tx: oneshot::Sender<RequestHandling>,
}

enum RequestHandling {
    LetThrough {
        to: SocketAddr,
        unchanged: Request<Incoming>,
    },
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

/// Two connections recovered after an HTTP upgrade occurred in [`FilteringService`].
pub struct UpgradedConnection {
    /// Client side - an HTTP client that tried to connect with agent's target.
    pub http_client_io: Upgraded,
    /// Server side - a stealer's client or an HTTP server running in the agent's target.
    pub http_server_io: UpgradedServerSide,
}

/// [`Service`] that matches incoming [`Request`]s against a set of stealer clients'
/// [`HttpFilter`]s. If a [`Request`] matches a filter for some client, this service responds with
/// what is provided by this client. If a [`Request`] does not match any filter, it is proxied to
/// its original destination.
#[derive(Clone)]
struct FilteringService {
    /// For sending incoming requests to the [`FilteredStealTask`].
    requests_tx: Sender<ExtractedRequest>,

    /// For recovering the upgraded connection in [`FilteredStealTask`].
    upgrade_tx: Sender<UpgradedConnection>,
}

impl FilteringService {
    fn bad_gateway(version: Version) -> Response<DynamicBody> {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .version(version)
            .body(BoxBody::new(Empty::new().map_err(|_| unreachable!())))
            .expect("creating an empty response should not fail")
    }

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
            .unwrap_or_else(|| Self::bad_gateway(version));

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

    async fn respond_with(
        &self,
        response: Response<DynamicBody>,
        on_upgrade: OnUpgrade,
        for_client: ClientId,
    ) -> Response<DynamicBody> {
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let upgrade_tx = self.upgrade_tx.clone();

            task::spawn(async move {
                let Ok(upgraded) = on_upgrade.await.inspect_err(|error| {
                    tracing::trace!(?error, client_id = for_client, "HTTP upgrade failed")
                }) else {
                    return;
                };

                let res = upgrade_tx
                    .send(UpgradedConnection {
                        http_client_io: upgraded,
                        http_server_io: UpgradedServerSide::MatchedClient(for_client),
                    })
                    .await;

                if res.is_err() {
                    tracing::trace!(client_id = for_client, "HTTP upgrade lost - channel closed");
                }
            });
        }

        response
    }

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
            }) => self.respond_with(response, on_upgrade, for_client).await,
            Err(..) => Self::bad_gateway(version),
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

pub struct FilteredStealTask<T> {
    connection_id: ConnectionId,
    original_destination: SocketAddr,

    filters: Arc<DashMap<ClientId, HttpFilter>>,
    clients: HashMap<ClientId, bool>,

    /// For receiving all incoming [`Request`]s. Sender belongs to related [`FilteringService`].
    requests_rx: Receiver<ExtractedRequest>,

    /// 1. Handle for the [`tokio::task`] that polls the [`hyper`] connection.
    /// 2. [`DropGuard`] for this [`tokio::task`], so that it abort when this struct is dropped.
    hyper_conn_task: (JoinHandle<Option<UpgradedConnection>>, DropGuard),

    /// Requests blocked on stealer clients' responses.
    blocked_requests: HashMap<(ClientId, RequestId), oneshot::Sender<RequestHandling>>,

    next_request_id: RequestId,

    task_rx: Receiver<ConnectionMessageIn>,
    task_tx: Sender<ConnectionMessageOut>,

    /// For safely downcasting stream after an HTTP upgrade. See [`Upgraded::downcast`].
    _io_type: PhantomData<fn() -> T>,
}

impl<T> FilteredStealTask<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Starts a new instance of this HTTP server.
    /// The server will manage the connection given as `io` and use the provided `filters` for
    /// matching incoming [`Request`]s with stealing clients.
    pub fn new(
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, HttpFilter>>,
        original_destination: SocketAddr,
        http_version: HttpVersion,
        io: T,
        rx: Receiver<ConnectionMessageIn>,
        tx: Sender<ConnectionMessageOut>,
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
            clients: Default::default(),
            requests_rx,
            hyper_conn_task: (task_handle, drop_guard),
            blocked_requests: Default::default(),
            next_request_id: Default::default(),
            task_rx: rx,
            task_tx: tx,
            _io_type: Default::default(),
        }
    }

    /// Matches the given [`Request`] against [`Self::filters`] and state of [`Self::clients`].
    fn match_request<B>(&self, request: &mut Request<B>) -> Option<ClientId> {
        self.filters
            .iter()
            .filter_map(|entry| entry.value().matches(request).then(|| *entry.key()))
            .find(|client_id| self.clients.get(client_id).copied().unwrap_or(true))
    }

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

    async fn handle_request(
        &mut self,
        mut request: ExtractedRequest,
    ) -> Result<(), ConnectionTaskError> {
        let Some(client_id) = self.match_request(&mut request.request) else {
            let _ = request
                .response_tx
                .send(RequestHandling::LetThrough {
                    to: self.original_destination,
                    unchanged: request.request,
                })
                .is_err();
            return Ok(());
        };

        if self.clients.insert(client_id, true).is_none() {
            self.task_tx
                .send(ConnectionMessageOut::SubscribedHttp {
                    client_id,
                    connection_id: self.connection_id,
                })
                .await?;
        }

        let id = self.next_request_id;
        self.next_request_id += 1;

        self.task_tx
            .send(ConnectionMessageOut::Request {
                client_id,
                connection_id: self.connection_id,
                request: request.request,
                id,
                port: self.original_destination.port(),
            })
            .await?;

        self.blocked_requests
            .insert((client_id, id), request.response_tx);

        self.clients.insert(client_id, true);

        Ok(())
    }

    pub async fn run(mut self) -> Result<(), ConnectionTaskError> {
        let mut queued_raw_data = vec![];

        loop {
            tokio::select! {
                message = self.task_rx.recv() => match message.ok_or(ConnectionTaskError::RecvError)? {
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
                        self.clients.insert(client_id, false);
                        self.blocked_requests.retain(|key, _| key.0 != client_id);
                    },
                },

                request = self.requests_rx.recv() => match request {
                    Some(request) => self.handle_request(request).await?,

                    None => break,
                }
            }
        }

        let Some(upgraded) = self
            .hyper_conn_task
            .0
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

                let subscribed = self.clients.remove(&client_id).unwrap_or(false);

                for (client_id, subscribed) in self.clients {
                    if subscribed {
                        self.task_tx
                            .send(ConnectionMessageOut::Closed {
                                client_id,
                                connection_id: self.connection_id,
                            })
                            .await?;
                    }
                }

                if subscribed {
                    return Ok(());
                }

                if !http_client_read_buf.is_empty() {
                    self.task_tx
                        .send(ConnectionMessageOut::Raw {
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
                    tx: self.task_tx,
                    rx: self.task_rx,
                }
                .run()
                .await
            }

            UpgradedServerSide::OriginalDestination(upgraded) => {
                tracing::trace!(
                    connection_id = self.connection_id,
                    "HTTP connection upgraded for original destination",
                );

                for (client_id, subscribed) in self.clients {
                    if subscribed {
                        self.task_tx
                            .send(ConnectionMessageOut::Closed {
                                connection_id: self.connection_id,
                                client_id,
                            })
                            .await?;
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
}
