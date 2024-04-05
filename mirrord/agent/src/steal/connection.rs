use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use fancy_regex::Regex;
use http_body_util::BodyExt;
use hyper::{
    body::Incoming,
    http::{header::UPGRADE, request::Parts},
    Request,
};
use mirrord_protocol::{
    tcp::{
        DaemonTcp, HttpRequest, HttpResponseFallback, InternalHttpBody, InternalHttpRequest,
        StealType, TcpClose, TcpData, HTTP_FILTERED_UPGRADE_VERSION, HTTP_FRAMED_VERSION,
    },
    ConnectionId, Port,
    RemoteError::{BadHttpFilterExRegex, BadHttpFilterRegex},
    RequestId,
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{AgentError, Result},
    steal::{
        connections::{
            ConnectionMessageIn, ConnectionMessageOut, StolenConnection, StolenConnections,
        },
        http::HttpFilter,
        orig_dst,
        subscriptions::{IpTablesRedirector, PortSubscriptions},
        Command, StealerCommand,
    },
    util::ClientId,
};

/// A stolen HTTP request that matched a client's filter.
#[derive(Debug)]
struct MatchedHttpRequest {
    connection_id: ConnectionId,
    port: Port,
    request_id: RequestId,
    request: Request<Incoming>,
}

impl MatchedHttpRequest {
    async fn into_serializable(self) -> Result<HttpRequest<InternalHttpBody>, hyper::Error> {
        let (
            Parts {
                method,
                uri,
                version,
                headers,
                ..
            },
            body,
        ) = self.request.into_parts();

        let body = InternalHttpBody::from_body(body).await?;

        let internal_request = InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        };

        Ok(HttpRequest {
            port: self.port,
            connection_id: self.connection_id,
            request_id: self.request_id,
            internal_request,
        })
    }

    async fn into_serializable_fallback(self) -> Result<HttpRequest<Vec<u8>>, hyper::Error> {
        let (
            Parts {
                method,
                uri,
                version,
                headers,
                ..
            },
            body,
        ) = self.request.into_parts();

        let body = body.collect().await?.to_bytes().to_vec();

        let internal_request = InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        };

        Ok(HttpRequest {
            port: self.port,
            connection_id: self.connection_id,
            request_id: self.request_id,
            internal_request,
        })
    }
}

/// A stealer client.
struct Client {
    /// For sending messages to client's [`TcpStealerApi`](super::api::TcpStealerApi).
    /// Comes to [`TcpConnectionStealer`] in [`Command::NewClient`].
    tx: Sender<DaemonTcp>,
    /// Clients [`mirrord_protocol`] verison.
    protocol_version: semver::Version,
    /// Client subscriptions to stolen connections.
    /// Used to unsubscribe when the client exits.
    subscribed_connections: HashSet<ConnectionId>,
}

impl Client {
    /// Attempts to spawn a new [`tokio::task`] to transform the given [`MatchedHttpRequest`] into
    /// [`DaemonTcp::HttpRequest`] or [`DaemonTcp::HttpRequestFramed`] and send it via cloned
    /// [`Client::tx`].
    ///
    /// Inspects [`Client::protocol_version`] to pick between [`DaemonTcp`] variants and check for
    /// upgrade requests.
    ///
    /// Returns `true` if the [`tokio::task`] was spawned.
    /// Otherwise, returns `false`. Currently, this is the case only when the given
    /// [`MatchedHttpRequest`] is an upgrade request, but client's protocol version does not match
    /// [`HTTP_FILTERED_UPGRADE_VERSION`].
    ///
    /// # Why async?
    ///
    /// This method spawns a [`tokio::task`] to read the [`Incoming`] body od the request without
    /// blocking the main [`TcpConnectionStealer`] loop.
    fn send_request_async(&self, request: MatchedHttpRequest) -> bool {
        if request.request.headers().contains_key(UPGRADE)
            && !HTTP_FILTERED_UPGRADE_VERSION.matches(&self.protocol_version)
        {
            return false;
        }

        let framed = HTTP_FRAMED_VERSION.matches(&self.protocol_version);
        let tx = self.tx.clone();

        tokio::spawn(async move {
            if framed {
                let Ok(request) = request.into_serializable().await else {
                    return;
                };

                let _ = tx.send(DaemonTcp::HttpRequestFramed(request)).await;
            } else {
                let Ok(request) = request.into_serializable_fallback().await else {
                    return;
                };

                let _ = tx.send(DaemonTcp::HttpRequest(request)).await;
            }
        });

        true
    }
}

/// Created once per agent during initialization.
///
/// Meant to be run (see [`TcpConnectionStealer::start`]) in a separate thread while the agent
/// lives. When handling port subscription requests, this struct manipulates iptables, so it should
/// run in the same network namespace as the agent's target.
pub(crate) struct TcpConnectionStealer {
    /// For managing active subscriptions and port redirections.
    port_subscriptions: PortSubscriptions<IpTablesRedirector>,

    /// For receiving commands.
    /// The other end of this channel belongs to [`TcpStealerApi`](super::api::TcpStealerApi).
    command_rx: Receiver<StealerCommand>,

    /// Connected [`Client`]s (layer instances).
    clients: HashMap<ClientId, Client>,

    /// Set of active connections stolen by [`Self::port_subscriptions`].
    connections: StolenConnections,
}

impl TcpConnectionStealer {
    pub const TASK_NAME: &'static str = "Stealer";

    /// Initializes a new [`TcpConnectionStealer`], but doesn't start the actual work.
    /// You need to call [`TcpConnectionStealer::start`] to do so.
    #[tracing::instrument(level = "trace")]
    pub(crate) async fn new(command_rx: Receiver<StealerCommand>) -> Result<Self, AgentError> {
        let port_subscriptions = {
            let flush_connections = std::env::var("MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS")
                .ok()
                .and_then(|var| var.parse::<bool>().ok())
                .unwrap_or_default();
            let redirector = IpTablesRedirector::new(flush_connections).await?;

            PortSubscriptions::new(redirector, 4)
        };

        Ok(Self {
            port_subscriptions,
            command_rx,
            clients: HashMap::with_capacity(8),
            connections: StolenConnections::with_capacity(8),
        })
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
    pub(crate) async fn start(
        mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), AgentError> {
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

                accept = self.port_subscriptions.next_connection() => match accept {
                    Ok((stream, peer)) => self.incoming_connection(stream, peer).await?,
                    Err(error) => {
                        tracing::error!(?error, "Failed to accept a stolen connection");
                        break Err(error);
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
    async fn incoming_connection(&mut self, stream: TcpStream, peer: SocketAddr) -> Result<()> {
        let mut real_address = orig_dst::orig_dst_addr(&stream)?;
        // If we use the original IP we would go through prerouting and hit a loop.
        // localhost should always work.
        real_address.set_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));

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
    async fn handle_connection_update(
        &mut self,
        update: ConnectionMessageOut,
    ) -> Result<(), AgentError> {
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
                port,
            } => {
                let Some(client) = self.clients.get(&client_id) else {
                    tracing::trace!(client_id, connection_id, "Client has already exited");
                    return Ok(());
                };

                if !client.subscribed_connections.contains(&connection_id) {
                    tracing::trace!(client_id, connection_id, "Client has already unsubscribed");
                    return Ok(());
                }

                let matched_request = MatchedHttpRequest {
                    connection_id,
                    request,
                    request_id: id,
                    port,
                };

                if !client.send_request_async(matched_request) {
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

    /// Helper function to handle [`Command::PortSubscribe`] messages.
    ///
    /// Inserts a subscription into [`Self::port_subscriptions`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn port_subscribe(&mut self, client_id: ClientId, port_steal: StealType) -> Result<()> {
        let spec = match port_steal {
            StealType::All(port) => Ok((port, None)),
            StealType::FilteredHttp(port, filter) => Regex::new(&format!("(?i){filter}"))
                .map(|regex| (port, Some(HttpFilter::Header(regex))))
                .map_err(|err| BadHttpFilterRegex(filter, err.to_string())),
            StealType::FilteredHttpEx(port, filter) => HttpFilter::try_from(&filter)
                .map(|filter| (port, Some(filter)))
                .map_err(|err| BadHttpFilterExRegex(filter, err.to_string())),
        };

        let res = match spec {
            Ok((port, filter)) => self.port_subscriptions.add(client_id, port, filter).await?,
            Err(e) => Err(e.into()),
        };

        let client = self.clients.get(&client_id).expect("client not found");
        let _ = client.tx.send(DaemonTcp::SubscribeResult(res)).await;

        Ok(())
    }

    /// Removes the client with `client_id` from our list of clients (layers), and also removes
    /// their subscriptions from [`Self::port_subscriptions`] and all their open
    /// connections.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn close_client(&mut self, client_id: ClientId) -> Result<(), AgentError> {
        self.port_subscriptions.remove_all(client_id).await?;

        let client = self.clients.remove(&client_id).expect("client not found");
        for connection in client.subscribed_connections.into_iter() {
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

        match response.into_hyper::<hyper::Error>() {
            Ok(response) => {
                self.connections
                    .send(
                        connection_id,
                        ConnectionMessageIn::Response {
                            client_id,
                            request_id,
                            response,
                        },
                    )
                    .await;
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    connection_id,
                    request_id,
                    client_id,
                    "Failed to transform client message into a hyper response",
                );

                self.connections
                    .send(
                        connection_id,
                        ConnectionMessageIn::ResponseFailed {
                            client_id,
                            request_id,
                        },
                    )
                    .await;
            }
        }
    }

    /// Handles [`Command`]s that were received by [`TcpConnectionStealer::command_rx`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), AgentError> {
        let StealerCommand { client_id, command } = command;

        match command {
            Command::NewClient(daemon_tx, protocol_version) => {
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
                self.port_subscribe(client_id, port_steal).await?
            }

            Command::PortUnsubscribe(port) => {
                self.port_subscriptions.remove(client_id, port).await?;
            }

            Command::ClientClose => self.close_client(client_id).await?,

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
