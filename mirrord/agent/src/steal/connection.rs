use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use fancy_regex::Regex;
use futures::{stream::FuturesUnordered, StreamExt};
use http::Request;
use http_body_util::BodyExt;
use hyper::{
    body::Incoming,
    http::{header::UPGRADE, request::Parts},
};
use mirrord_agent_env::envs;
use mirrord_protocol::{
    batched_body::{BatchedBody, Frames},
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestErrorV1, ChunkedRequestErrorV2,
        ChunkedRequestStartV1, ChunkedRequestStartV2, DaemonTcp, HttpRequest, HttpRequestMetadata,
        HttpRequestTransportType, InternalHttpBody, InternalHttpBodyFrame, InternalHttpBodyNew,
        InternalHttpRequest, StealType, TcpClose, TcpData, HTTP_CHUNKED_REQUEST_V2_VERSION,
        HTTP_CHUNKED_REQUEST_VERSION, HTTP_FILTERED_UPGRADE_VERSION, HTTP_FRAMED_VERSION,
    },
    ConnectionId,
    RemoteError::{BadHttpFilterExRegex, BadHttpFilterRegex},
    RequestId,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{error::SendError, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use super::{http::HttpResponseFallback, subscriptions::PortRedirector, tls::StealTlsHandlerStore};
use crate::{
    error::{AgentError, AgentResult},
    metrics::HTTP_REQUEST_IN_PROGRESS_COUNT,
    steal::{
        connections::{
            ConnectionMessageIn, ConnectionMessageOut, StolenConnection, StolenConnections,
        },
        http::HttpFilter,
        orig_dst,
        subscriptions::{IpTablesRedirector, PortSubscriptions},
        Command, StealerCommand,
    },
    util::{ChannelClosedFuture, ClientId},
};

/// A stolen HTTP request that matched a client's filter.
#[derive(Debug)]
struct MatchedHttpRequest {
    connection_id: ConnectionId,
    request_id: RequestId,
    request: Request<Incoming>,
    metadata: HttpRequestMetadata,
    transport: HttpRequestTransportType,
}

impl MatchedHttpRequest {
    fn new(
        connection_id: ConnectionId,
        request_id: RequestId,
        request: Request<Incoming>,
        metadata: HttpRequestMetadata,
        transport: HttpRequestTransportType,
    ) -> Self {
        HTTP_REQUEST_IN_PROGRESS_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Self {
            connection_id,
            request_id,
            request,
            metadata,
            transport,
        }
    }
}

/// Errors that can occur when we try to send a stolen request to the client.
#[derive(Error, Debug)]
enum PassRequestError {
    #[error("failed to read request body: {0}")]
    ReadBodyError(#[from] hyper::Error),
    #[error("client disonnected")]
    ClientDisconnected,
}

impl From<SendError<DaemonTcp>> for PassRequestError {
    fn from(_: SendError<DaemonTcp>) -> Self {
        Self::ClientDisconnected
    }
}

/// A stealer client.
struct Client {
    /// For sending messages to client's [`TcpStealerApi`](super::api::TcpStealerApi).
    /// Comes to [`TcpConnectionStealer`] in [`Command::NewClient`].
    tx: Sender<DaemonTcp>,
    /// Client's [`mirrord_protocol`] verison.
    ///
    /// Determines which variant of [`DaemonTcp`] we use to send stolen HTTP requests.
    protocol_version: semver::Version,
    /// Client subscriptions to stolen connections.
    /// Used to unsubscribe when the client exits.
    subscribed_connections: HashSet<ConnectionId>,
}

impl Client {
    #[tracing::instrument(level = Level::DEBUG, skip(tx), err(level = Level::DEBUG))]
    async fn send_legacy(
        request: MatchedHttpRequest,
        tx: Sender<DaemonTcp>,
    ) -> Result<(), PassRequestError> {
        let (
            Parts {
                method,
                uri,
                version,
                headers,
                ..
            },
            body,
        ) = request.request.into_parts();

        let body = body.collect().await?.to_bytes().to_vec();

        let internal_request = InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        };

        let HttpRequestMetadata::V1 { destination, .. } = request.metadata;

        let request = HttpRequest {
            port: destination.port(),
            connection_id: request.connection_id,
            request_id: request.request_id,
            internal_request,
        };

        tx.send(DaemonTcp::HttpRequest(request)).await?;

        Ok(())
    }

    #[tracing::instrument(level = Level::DEBUG, skip(tx), err(level = Level::DEBUG))]
    async fn send_framed(
        request: MatchedHttpRequest,
        tx: Sender<DaemonTcp>,
    ) -> Result<(), PassRequestError> {
        let (
            Parts {
                method,
                uri,
                version,
                headers,
                ..
            },
            body,
        ) = request.request.into_parts();

        let body = InternalHttpBody::from_body(body).await?;

        let internal_request = InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        };

        let HttpRequestMetadata::V1 { destination, .. } = request.metadata;

        let request = HttpRequest {
            port: destination.port(),
            connection_id: request.connection_id,
            request_id: request.request_id,
            internal_request,
        };

        tx.send(DaemonTcp::HttpRequestFramed(request)).await?;

        Ok(())
    }

    #[tracing::instrument(level = Level::DEBUG, skip(tx), err(level = Level::DEBUG))]
    async fn send_chunked(
        request: MatchedHttpRequest,
        use_v2: bool,
        tx: Sender<DaemonTcp>,
    ) -> Result<(), PassRequestError> {
        let (
            Parts {
                method,
                uri,
                version,
                headers,
                ..
            },
            mut body,
        ) = request.request.into_parts();

        let Frames { frames, is_last } = body.ready_frames()?;

        let frames = frames
            .into_iter()
            .map(InternalHttpBodyFrame::from)
            .collect();

        let message = if use_v2 {
            ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                connection_id: request.connection_id,
                request_id: request.request_id,
                metadata: request.metadata,
                transport: request.transport,
                request: InternalHttpRequest {
                    method,
                    uri,
                    headers,
                    version,
                    body: InternalHttpBodyNew { frames, is_last },
                },
            })
        } else {
            let HttpRequestMetadata::V1 { destination, .. } = request.metadata;
            ChunkedRequest::StartV1(ChunkedRequestStartV1 {
                connection_id: request.connection_id,
                request_id: request.request_id,
                port: destination.port(),
                internal_request: InternalHttpRequest {
                    method,
                    uri,
                    headers,
                    version,
                    body: frames,
                },
            })
        };

        tx.send(DaemonTcp::HttpRequestChunked(message)).await?;

        if is_last {
            if use_v2 {
                return Ok(());
            }

            let message =
                DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    frames: Default::default(),
                    is_last: true,
                }));
            tx.send(message).await?;
            return Ok(());
        }

        loop {
            let Frames { frames, is_last } = match body.next_frames().await {
                Ok(frames) => frames,
                Err(error) => {
                    let message = if use_v2 {
                        ChunkedRequest::ErrorV2(ChunkedRequestErrorV2 {
                            connection_id: request.connection_id,
                            request_id: request.request_id,
                            error_message: error.to_string(),
                        })
                    } else {
                        ChunkedRequest::ErrorV1(ChunkedRequestErrorV1 {
                            connection_id: request.connection_id,
                            request_id: request.request_id,
                        })
                    };

                    let _ = tx.send(DaemonTcp::HttpRequestChunked(message)).await;

                    break Err(error.into());
                }
            };

            let frames = frames
                .into_iter()
                .map(InternalHttpBodyFrame::from)
                .collect();

            let message =
                DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                    frames,
                    is_last,
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                }));

            tx.send(message).await?;

            if is_last {
                break Ok(());
            }
        }
    }

    /// Attempts to spawn a new [`tokio::task`] to transform the given [`MatchedHttpRequest`] into
    /// [`DaemonTcp::HttpRequest`], [`DaemonTcp::HttpRequestFramed`] or
    /// [`DaemonTcp::HttpRequestChunked`] and send it via cloned [`Client::tx`].
    ///
    /// Inspects [`Client::protocol_version`] to pick between [`DaemonTcp`] variants and check for
    /// upgrade requests.
    ///
    /// Returns `true` if the [`tokio::task`] was spawned.
    /// Otherwise, returns `false`. Currently, this is the case only when the given
    /// [`MatchedHttpRequest`] is an upgrade request, but client's protocol version does not match
    /// [`HTTP_FILTERED_UPGRADE_VERSION`].
    ///
    /// # Why in the background?
    ///
    /// This method spawns a [`tokio::task`] to read the [`Incoming`] body od the request without
    /// blocking the main [`TcpConnectionStealer`] loop.
    ///
    /// # Tracing
    ///
    /// The spawned background task calls one of `send_` helper methods.
    /// These can fail only when we encounter an error while reading request body, or when the
    /// client disconnects. Both cases are quite normal, so don't log errors higher than on
    /// `DEBUG`.
    fn send_request_in_bg(&self, request: MatchedHttpRequest) -> bool {
        let is_upgrade = request.request.headers().contains_key(UPGRADE);
        let is_tls = matches!(request.transport, HttpRequestTransportType::Tls);

        if is_upgrade && !HTTP_FILTERED_UPGRADE_VERSION.matches(&self.protocol_version) {
            return false;
        }

        if is_tls && !HTTP_CHUNKED_REQUEST_V2_VERSION.matches(&self.protocol_version) {
            return false;
        }

        let framed = HTTP_FRAMED_VERSION.matches(&self.protocol_version);
        let chunked = HTTP_CHUNKED_REQUEST_VERSION.matches(&self.protocol_version);
        let chunked_v2 = HTTP_CHUNKED_REQUEST_V2_VERSION.matches(&self.protocol_version);

        let tx = self.tx.clone();

        tracing::trace!(
            ?request,
            client_protocol_version = %self.protocol_version,
            "Sending stolen request to the client",
        );

        tokio::spawn(async move {
            let _ = if chunked {
                Self::send_chunked(request, chunked_v2, tx).await
            } else if framed {
                Self::send_framed(request, tx).await
            } else {
                Self::send_legacy(request, tx).await
            };
        });

        true
    }
}

struct TcpStealerConfig {
    stealer_flush_connections: bool,
    pod_ips: Vec<IpAddr>,
}

impl TcpStealerConfig {
    fn from_env() -> Self {
        Self {
            stealer_flush_connections: envs::STEALER_FLUSH_CONNECTIONS.from_env_or_default(),
            pod_ips: envs::POD_IPS.from_env_or_default(),
        }
    }
}

/// Created once per agent during initialization.
///
/// Meant to be run (see [`TcpConnectionStealer::start`]) in a separate thread while the agent
/// lives. When handling port subscription requests, this struct manipulates iptables, so it should
/// run in the same network namespace as the agent's target.
///
/// Enabled by the `steal` feature for incoming traffic.
pub(crate) struct TcpConnectionStealer<Redirector: PortRedirector = IpTablesRedirector> {
    /// For managing active subscriptions and port redirections.
    port_subscriptions: PortSubscriptions<Redirector>,

    /// For receiving commands.
    /// The other end of this channel belongs to [`TcpStealerApi`](super::api::TcpStealerApi).
    command_rx: Receiver<StealerCommand>,

    /// Connected [`Client`]s (layer instances).
    clients: HashMap<ClientId, Client>,

    /// [`Future`](std::future::Future)s that resolve when stealer clients close.
    clients_closed: FuturesUnordered<ChannelClosedFuture>,

    /// Set of active connections stolen by [`Self::port_subscriptions`].
    connections: StolenConnections,

    /// Shen set, the stealer will use IPv6 if needed.
    support_ipv6: bool,
}

impl TcpConnectionStealer<IpTablesRedirector> {
    pub const TASK_NAME: &'static str = "Stealer";

    /// Initializes a new [`TcpConnectionStealer`], but doesn't start the actual work.
    /// You need to call [`TcpConnectionStealer::start`] to do so.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(crate) async fn new(
        command_rx: Receiver<StealerCommand>,
        support_ipv6: bool,
        tls_handler_store: Option<StealTlsHandlerStore>,
    ) -> AgentResult<Self> {
        let config = TcpStealerConfig::from_env();
        let redirector = IpTablesRedirector::new(
            config.stealer_flush_connections,
            config.pod_ips,
            support_ipv6,
        )
        .await?;

        Ok(Self::with_redirector(
            command_rx,
            support_ipv6,
            redirector,
            tls_handler_store,
        ))
    }
}

impl<Redirector> TcpConnectionStealer<Redirector>
where
    Redirector: PortRedirector,
    Redirector::Error: std::error::Error + Into<AgentError>,
    AgentError: From<Redirector::Error>,
{
    /// Creates a new stealer.
    ///
    /// Given [`PortRedirector`] will be used to capture incoming connections.
    pub(crate) fn with_redirector(
        command_rx: Receiver<StealerCommand>,
        support_ipv6: bool,
        redirector: Redirector,
        tls_handler_store: Option<StealTlsHandlerStore>,
    ) -> Self {
        Self {
            port_subscriptions: PortSubscriptions::new(redirector, 4),
            command_rx,
            clients: HashMap::with_capacity(8),
            clients_closed: Default::default(),
            connections: StolenConnections::new(8, tls_handler_store),
            support_ipv6,
        }
    }

    /// Runs the tcp traffic stealer loop.
    ///
    /// The loop deals with 5 different paths:
    ///
    /// 1. Receiving a new [`StealerCommand`];
    ///
    /// 2. Accepting a new stolen connection based on the clients' port subscriptions;
    ///
    /// 3. Receiving an update from one of the active stolen connections;
    ///
    /// 4. Handling the cancellation of the whole stealer thread (given `cancellation_token`).
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn start(mut self, cancellation_token: CancellationToken) -> AgentResult<()> {
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break Ok(());
                    };

                    self.handle_command(command)
                        .await
                        .inspect_err(|error| tracing::error!(?error, "Failed handling command"))?;
                },

                Some(client_id) = self.clients_closed.next() => {
                    self.close_client(client_id).await?;
                },

                accept = self.port_subscriptions.next_connection() => match accept {
                    Ok((stream, peer)) => {
                        self.incoming_connection(stream, peer).await?;
                    }
                    Err(error) => {
                        tracing::error!(?error, "Failed to accept a stolen connection");
                        break Err(error.into());
                    }
                },

                update = self.connections.wait() => self.handle_connection_update(update).await?,

                _ = cancellation_token.cancelled() => {
                    break Ok(());
                }
            }
        }
    }

    /// Handles a new remote connection that was stolen by [`Self::port_subscriptions`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn incoming_connection(
        &mut self,
        stream: TcpStream,
        peer: SocketAddr,
    ) -> AgentResult<()> {
        let mut real_address = orig_dst::orig_dst_addr(&stream)?;
        let localhost = if self.support_ipv6 && real_address.is_ipv6() {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        // If we use the original IP we would go through prerouting and hit a loop.
        // localhost should always work.
        real_address.set_ip(localhost);

        let Some(port_subscription) = self.port_subscriptions.get(real_address.port()).cloned()
        else {
            // Got connection to port without subscribers.
            // This *can* happen due to race conditions
            // (e.g. we just processed an `unsubscribe` command, but our stealer socket already had
            // a connection queued)
            // Here we just drop the TcpStream between our stealer socket and the remote peer (one
            // that attempted to connect with our target) and the connection is closed.
            return Ok(());
        };

        let stolen_connection = StolenConnection {
            stream,
            source: peer,
            destination: real_address,
            port_subscription,
        };

        self.connections.manage(stolen_connection);

        Ok(())
    }

    /// Handles an update from one of the connections in [`Self::connections`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_connection_update(&mut self, update: ConnectionMessageOut) -> AgentResult<()> {
        match update {
            ConnectionMessageOut::Closed {
                connection_id,
                client_id,
            } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    tracing::trace!(client_id, connection_id, "Client has already exited");
                    return Ok(());
                };

                if !client.subscribed_connections.remove(&connection_id) {
                    tracing::trace!(client_id, connection_id, "Client has already unsubscribed");
                    return Ok(());
                }

                let _ = client
                    .tx
                    .send(DaemonTcp::Close(TcpClose { connection_id }))
                    .await;
            }

            ConnectionMessageOut::SubscribedTcp {
                client_id,
                connection,
            } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    tracing::trace!(
                        client_id,
                        connection_id = connection.connection_id,
                        "Client has already exited"
                    );
                    self.connections
                        .send(
                            connection.connection_id,
                            ConnectionMessageIn::Unsubscribed { client_id },
                        )
                        .await;
                    return Ok(());
                };

                client
                    .subscribed_connections
                    .insert(connection.connection_id);

                let _ = client.tx.send(DaemonTcp::NewConnection(connection)).await;
            }

            ConnectionMessageOut::SubscribedHttp {
                client_id,
                connection_id,
            } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    tracing::trace!(client_id, connection_id, "Client has already exited");
                    self.connections
                        .send(
                            connection_id,
                            ConnectionMessageIn::Unsubscribed { client_id },
                        )
                        .await;
                    return Ok(());
                };

                client.subscribed_connections.insert(connection_id);
            }

            ConnectionMessageOut::Raw {
                client_id,
                connection_id,
                data,
            } => {
                let Some(client) = self.clients.get(&client_id) else {
                    tracing::trace!(client_id, connection_id, "Client has already exited");
                    return Ok(());
                };

                if !client.subscribed_connections.contains(&connection_id) {
                    tracing::trace!(client_id, connection_id, "Client has already unsubscribed");
                    return Ok(());
                }

                let _ = client
                    .tx
                    .send(DaemonTcp::Data(TcpData {
                        connection_id,
                        bytes: data,
                    }))
                    .await;
            }

            ConnectionMessageOut::Request {
                client_id,
                connection_id,
                request,
                id,
                metadata,
                transport,
            } => {
                let Some(client) = self.clients.get(&client_id) else {
                    tracing::trace!(client_id, connection_id, "Client has already exited");
                    return Ok(());
                };

                if !client.subscribed_connections.contains(&connection_id) {
                    tracing::trace!(client_id, connection_id, "Client has already unsubscribed");
                    return Ok(());
                }

                let matched_request =
                    MatchedHttpRequest::new(connection_id, id, request, metadata, transport);

                if !client.send_request_in_bg(matched_request) {
                    self.connections
                        .send(
                            connection_id,
                            ConnectionMessageIn::ResponseFailed {
                                client_id,
                                request_id: id,
                            },
                        )
                        .await;
                }
            }
        }

        Ok(())
    }

    /// Helper function to handle [`Command::PortSubscribe`] messages for the `TcpStealer`.
    ///
    /// Checks if [`StealType`] is a valid [`HttpFilter`], then inserts a subscription into
    /// [`Self::port_subscriptions`].
    ///
    /// - Returns: `true` if this is an HTTP filtered subscription.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    async fn port_subscribe(
        &mut self,
        client_id: ClientId,
        port_steal: StealType,
    ) -> AgentResult<bool> {
        let spec = match port_steal {
            StealType::All(port) => Ok((port, None)),
            StealType::FilteredHttp(port, filter) => Regex::new(&format!("(?i){filter}"))
                .map(|regex| (port, Some(HttpFilter::Header(regex))))
                .map_err(|err| BadHttpFilterRegex(filter, err.to_string())),
            StealType::FilteredHttpEx(port, filter) => HttpFilter::try_from(&filter)
                .map(|filter| (port, Some(filter)))
                .map_err(|err| BadHttpFilterExRegex(filter, err.to_string())),
        };

        let filtered = spec
            .as_ref()
            .map(|(_, filter)| filter.is_some())
            .unwrap_or_default();

        let res = match spec {
            Ok((port, filter)) => self.port_subscriptions.add(client_id, port, filter).await?,
            Err(e) => Err(e.into()),
        };

        let client = self.clients.get(&client_id).expect("client not found");
        let _ = client.tx.send(DaemonTcp::SubscribeResult(res)).await;

        Ok(filtered)
    }

    /// Removes the client with `client_id` from our list of clients (layers), and also removes
    /// their subscriptions from [`Self::port_subscriptions`] and all their open
    /// connections.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn close_client(&mut self, client_id: ClientId) -> AgentResult<()> {
        self.port_subscriptions.remove_all(client_id).await?;

        let client = self.clients.remove(&client_id).expect("client not found");
        for connection in client.subscribed_connections {
            self.connections
                .send(connection, ConnectionMessageIn::Unsubscribed { client_id })
                .await;
        }

        Ok(())
    }

    /// Converts the given [`HttpResponseFallback`] to a [`hyper::Response`] and sends it to
    /// [`StolenConnections`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn send_http_response(&mut self, client_id: ClientId, response: HttpResponseFallback) {
        let connection_id = response.connection_id();
        let request_id = response.request_id();
        self.connections
            .send(
                connection_id,
                ConnectionMessageIn::Response {
                    client_id,
                    request_id,
                    response: response.into_hyper::<hyper::Error>(),
                },
            )
            .await;
    }

    /// Handles [`Command`]s that were received by [`TcpConnectionStealer::command_rx`].
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    async fn handle_command(&mut self, command: StealerCommand) -> AgentResult<()> {
        let StealerCommand { client_id, command } = command;

        match command {
            Command::NewClient(daemon_tx, protocol_version) => {
                self.clients_closed
                    .push(ChannelClosedFuture::new(daemon_tx.clone(), client_id));
                self.clients.insert(
                    client_id,
                    Client {
                        tx: daemon_tx,
                        protocol_version,
                        subscribed_connections: Default::default(),
                    },
                );
            }

            Command::ConnectionUnsubscribe(connection_id) => {
                self.clients
                    .get_mut(&client_id)
                    .expect("client not found")
                    .subscribed_connections
                    .remove(&connection_id);

                self.connections
                    .send(
                        connection_id,
                        ConnectionMessageIn::Unsubscribed { client_id },
                    )
                    .await;
            }

            Command::PortSubscribe(port_steal) => {
                self.port_subscribe(client_id, port_steal).await?;
            }

            Command::PortUnsubscribe(port) => {
                self.port_subscriptions.remove(client_id, port).await?;
            }

            Command::ResponseData(TcpData {
                connection_id,
                bytes,
            }) => {
                self.connections
                    .send(
                        connection_id,
                        ConnectionMessageIn::Raw {
                            client_id,
                            data: bytes,
                        },
                    )
                    .await;
            }

            Command::HttpResponse(response) => {
                self.send_http_response(client_id, response).await;
            }

            Command::SwitchProtocolVersion(new_version) => {
                let client = self.clients.get_mut(&client_id).expect("client not found");
                client.protocol_version = new_version;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{net::SocketAddr, time::Duration};

    use bytes::Bytes;
    use futures::{future::BoxFuture, FutureExt};
    use http::{Method, Request, Response, Version};
    use http_body_util::{Empty, StreamBody};
    use hyper::{
        body::{Frame, Incoming},
        service::Service,
    };
    use hyper_util::rt::TokioIo;
    use mirrord_protocol::{
        tcp::{
            ChunkedRequest, DaemonTcp, Filter, HttpFilter, HttpRequestMetadata,
            HttpRequestTransportType, InternalHttpBodyFrame, StealType,
        },
        Port,
    };
    use rstest::rstest;
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::{
            mpsc::{self, Receiver, Sender},
            oneshot, watch,
        },
    };
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_util::sync::CancellationToken;

    use super::AgentError;
    use crate::{
        steal::{
            connection::{Client, MatchedHttpRequest},
            subscriptions::PortRedirector,
            TcpConnectionStealer, TcpStealerApi,
        },
        watched_task::TaskStatus,
    };

    /// Notification about a requested redirection operation.
    ///
    /// Produced by [`NotifyingRedirector`].
    #[derive(Debug, PartialEq, Eq)]
    enum RedirectNotification {
        Added(Port),
        Removed(Port),
        Cleanup,
    }

    /// Test [`PortRedirector`] that never fails and notifies about requested operations using an
    /// [`mpsc::channel`].
    struct NotifyingRedirector(Sender<RedirectNotification>);

    #[async_trait::async_trait]
    impl PortRedirector for NotifyingRedirector {
        type Error = AgentError;

        async fn add_redirection(&mut self, port: Port) -> Result<(), Self::Error> {
            self.0
                .send(RedirectNotification::Added(port))
                .await
                .unwrap();
            Ok(())
        }

        async fn remove_redirection(&mut self, port: Port) -> Result<(), Self::Error> {
            self.0
                .send(RedirectNotification::Removed(port))
                .await
                .unwrap();
            Ok(())
        }

        async fn cleanup(&mut self) -> Result<(), Self::Error> {
            self.0.send(RedirectNotification::Cleanup).await.unwrap();
            Ok(())
        }

        async fn next_connection(&mut self) -> Result<(TcpStream, SocketAddr), Self::Error> {
            std::future::pending().await
        }
    }

    async fn prepare_dummy_service() -> (
        SocketAddr,
        Receiver<(Request<Incoming>, oneshot::Sender<Response<Empty<Bytes>>>)>,
    ) {
        type ReqSender = Sender<(Request<Incoming>, oneshot::Sender<Response<Empty<Bytes>>>)>;
        struct DummyService {
            tx: ReqSender,
        }

        impl Service<Request<Incoming>> for DummyService {
            type Response = Response<Empty<Bytes>>;

            type Error = hyper::Error;

            type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

            fn call(&self, req: Request<Incoming>) -> Self::Future {
                let tx = self.tx.clone();
                async move {
                    let (res_tx, res_rx) = oneshot::channel();
                    tx.send((req, res_tx)).await.unwrap();
                    Ok(res_rx.await.unwrap())
                }
                .boxed()
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(4);

        tokio::spawn(async move {
            loop {
                let (conn, _) = listener.accept().await.unwrap();
                let tx = tx.clone();
                tokio::spawn(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(conn), DummyService { tx }),
                );
            }
        });

        (server_address, rx)
    }

    #[tokio::test]
    async fn test_streaming_response() {
        let (addr, mut request_rx) = prepare_dummy_service().await;
        let conn = TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(conn))
            .await
            .unwrap();
        tokio::spawn(conn);

        let (body_tx, body_rx) = mpsc::channel::<hyper::Result<Frame<Bytes>>>(12);
        let body = StreamBody::new(ReceiverStream::new(body_rx));

        // Send a frame to be ready in ChunkedRequest::Start before hyper sender is used
        body_tx
            .send(Ok(Frame::data(b"string".to_vec().into())))
            .await
            .unwrap();

        tokio::spawn(
            sender.send_request(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .version(Version::HTTP_11)
                    .body(body)
                    .unwrap(),
            ),
        );

        let (client_tx, mut client_rx) = mpsc::channel::<DaemonTcp>(4);
        let client = Client {
            tx: client_tx,
            protocol_version: "1.7.0".parse().unwrap(),
            subscribed_connections: Default::default(),
        };

        let (request, response_tx) = request_rx.recv().await.unwrap();
        client.send_request_in_bg(MatchedHttpRequest {
            connection_id: 0,
            metadata: HttpRequestMetadata::V1 {
                source: "1.3.3.7:1337".parse().unwrap(),
                destination: "2.1.3.7:80".parse().unwrap(),
            },
            transport: HttpRequestTransportType::Tcp,
            request_id: 0,
            request,
        });

        // Verify that single-framed ChunkedRequest::Start requests are as expected, containing any
        // ready frames that were sent before Request was first sent
        let msg = client_rx.recv().await.unwrap();
        let DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV1(x)) = msg else {
            panic!("unexpected type received: {msg:?}")
        };
        assert_eq!(
            x.internal_request.body,
            vec![InternalHttpBodyFrame::Data(b"string".to_vec())]
        );
        let x = client_rx.recv().now_or_never();
        assert!(x.is_none());

        // Verify that single-framed ChunkedRequest::Body requests are as expected
        body_tx
            .send(Ok(Frame::data(b"another_string".to_vec().into())))
            .await
            .unwrap();
        let msg = client_rx.recv().await.unwrap();
        let DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(x)) = msg else {
            panic!("unexpected type received: {msg:?}")
        };
        assert_eq!(
            x.frames,
            vec![InternalHttpBodyFrame::Data(b"another_string".to_vec())]
        );
        let x = client_rx.recv().now_or_never();
        assert!(x.is_none());

        let _ = response_tx.send(Response::new(Empty::default()));
    }

    #[rstest]
    #[tokio::test]
    #[timeout(std::time::Duration::from_secs(5))]
    async fn test_empty_streaming_request() {
        let (addr, mut request_rx) = prepare_dummy_service().await;
        let conn = TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(conn))
            .await
            .unwrap();
        tokio::spawn(conn);

        tokio::spawn(
            sender.send_request(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .version(Version::HTTP_11)
                    .body(http_body_util::Empty::<Bytes>::new())
                    .unwrap(),
            ),
        );

        let (client_tx, mut client_rx) = mpsc::channel::<DaemonTcp>(4);
        let client = Client {
            tx: client_tx,
            protocol_version: "1.7.0".parse().unwrap(),
            subscribed_connections: Default::default(),
        };

        let (request, response_tx) = request_rx.recv().await.unwrap();
        client.send_request_in_bg(MatchedHttpRequest {
            connection_id: 0,
            metadata: HttpRequestMetadata::V1 {
                source: "1.3.3.7:1337".parse().unwrap(),
                destination: "2.1.3.7:80".parse().unwrap(),
            },
            transport: HttpRequestTransportType::Tcp,
            request_id: 0,
            request,
        });

        // Verify that ChunkedRequest::Start request is as expected
        let msg = client_rx.recv().await.unwrap();
        let DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV1(_)) = msg else {
            panic!("unexpected type received: {msg:?}")
        };

        // Verify that empty ChunkedRequest::Body request is as expected
        let msg = client_rx.recv().await.unwrap();
        let DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(x)) = msg else {
            panic!("unexpected type received: {msg:?}")
        };
        assert_eq!(x.frames, vec![]);
        assert!(x.is_last);
        let x = client_rx.recv().now_or_never();
        assert!(x.is_none());

        let _ = response_tx.send(Response::new(Empty::default()));
    }

    /// Verifies that [`TcpConnectionStealer`] removes client's port subscriptions
    /// when client's [`TcpStealerApi`] is dropped.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn cleanup_on_client_closed() {
        let (command_tx, command_rx) = mpsc::channel(8);
        let (redirect_tx, mut redirect_rx) = mpsc::channel(2);
        let stealer = TcpConnectionStealer::with_redirector(
            command_rx,
            false,
            NotifyingRedirector(redirect_tx),
            None,
        );

        tokio::spawn(stealer.start(CancellationToken::new()));

        let (_dummy_tx, dummy_rx) = watch::channel(None);
        let task_status = TaskStatus::dummy(TcpConnectionStealer::TASK_NAME, dummy_rx);
        let mut api = TcpStealerApi::new(
            0,
            command_tx.clone(),
            task_status,
            8,
            mirrord_protocol::VERSION.clone(),
        )
        .await
        .unwrap();

        api.port_subscribe(StealType::FilteredHttpEx(
            80,
            HttpFilter::Header(Filter::new("user: test".into()).unwrap()),
        ))
        .await
        .unwrap();

        let response = api.recv().await.unwrap();
        assert_eq!(response, DaemonTcp::SubscribeResult(Ok(80)));

        let notification = redirect_rx.recv().await.unwrap();
        assert_eq!(notification, RedirectNotification::Added(80));

        std::mem::drop(api);

        let notification = redirect_rx.recv().await.unwrap();
        assert_eq!(notification, RedirectNotification::Removed(80));
    }
}
