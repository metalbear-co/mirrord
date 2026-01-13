use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::Entry},
    fmt,
    ops::Not,
};

use futures::{StreamExt, stream::FuturesUnordered};
use http::header::UPGRADE;
use mirrord_protocol::{
    LogMessage,
    tcp::{
        HTTP_CHUNKED_REQUEST_V2_VERSION, HTTP_FILTERED_UPGRADE_VERSION, MODE_AGNOSTIC_HTTP_REQUESTS,
    },
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use super::{
    Command, StealerCommand, StealerMessage,
    subscriptions::{PortSubscription, PortSubscriptions},
};
use crate::{
    http::filter::HttpFilter,
    incoming::{RedirectedHttp, RedirectedTcp, RedirectorTaskError, StealHandle, StolenTraffic},
    util::{ChannelClosedFuture, ClientId, protocol_version::ClientProtocolVersion},
};

/// Background task responsible for handling steal port subscriptions
/// and distributing stolen traffic between the agent clients.
///
/// Uses a [`StealHandle`] internally - traffic stealing is based on port redirections.
pub struct TcpStealerTask {
    /// Set of clients' active subscriptions.
    subscriptions: PortSubscriptions,
    /// Used to receive commands from the clients.
    command_rx: mpsc::Receiver<StealerCommand>,
    /// Currently connected clients.
    clients: HashMap<ClientId, Client>,
    /// Futures that resolve when clients disconnect (drop their [`StealerMessage`] receivers).
    disconnected_clients: FuturesUnordered<ChannelClosedFuture>,
    /// For tracking http requests whose bodies are being buffered
    ongoing_requests: JoinSet<RedirectedHttp>,
}

impl TcpStealerTask {
    pub fn new(command_rx: mpsc::Receiver<StealerCommand>, handle: StealHandle) -> Self {
        Self {
            subscriptions: PortSubscriptions::new(handle),
            command_rx,
            clients: Default::default(),
            disconnected_clients: Default::default(),
            ongoing_requests: Default::default(),
        }
    }

    pub async fn run(mut self, token: CancellationToken) -> Result<(), RedirectorTaskError> {
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    self.handle_command(command).await?;
                }

                Some(result) = self.subscriptions.next() => {
                    let (traffic, subscription) = result?;
                    Self::handle_stolen_traffic(&self.clients, traffic, subscription, &mut self.ongoing_requests).await;
                }

                Some(client_id) = self.disconnected_clients.next() => {
                    self.handle_client_disconnected(client_id);
                }

                Some(next) = self.ongoing_requests.join_next() => {
                    match next {
                        Ok(http) => {
                            self.handle_buffered_http(http).await;
                        },
                        Err(error) => {
                            tracing::error!(
                                ?error,
                                "HTTP body buffer task panicked. This is a bug in the agent, please report it"
                            );
                        },
                    }
                }

                _ = token.cancelled() => break,
            }
        }

        Ok(())
    }

    /// Returns a [`semver::VersionReq`] for the given subscription and stolen traffic.
    ///
    /// Client's [`mirrord_protocol`] version must match the requirement
    /// for the client to receive this traffic. Otherwise, the client will not be able to handle
    /// the traffic.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn protocol_version_req_tcp(
        subscription: &PortSubscription,
        tcp: &RedirectedTcp,
    ) -> Cow<'static, semver::VersionReq> {
        tcp.info()
            .tls_connector
            .is_some()
            .then_some(&*MODE_AGNOSTIC_HTTP_REQUESTS)
            .map(Cow::Borrowed)
            .unwrap_or(Cow::Owned(semver::VersionReq::STAR))
    }

    /// Returns a [`semver::VersionReq`] for the given subscription and stolen traffic.
    ///
    /// Client's [`mirrord_protocol`] version must match the requirement
    /// for the client to receive this traffic. Otherwise, the client will not be able to handle
    /// the traffic.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn protocol_version_req_http(
        subscription: &PortSubscription,
        http: &RedirectedHttp,
    ) -> Cow<'static, semver::VersionReq> {
        matches!(subscription, PortSubscription::Unfiltered(..))
            .then_some(&*MODE_AGNOSTIC_HTTP_REQUESTS)
            .or_else(|| {
                http.info()
                    .tls_connector
                    .is_some()
                    .then_some(&*HTTP_CHUNKED_REQUEST_V2_VERSION)
            })
            .or_else(|| {
                http.parts()
                    .headers
                    .contains_key(UPGRADE)
                    .then_some(&*HTTP_FILTERED_UPGRADE_VERSION)
            })
            .map(Cow::Borrowed)
            .unwrap_or(Cow::Owned(semver::VersionReq::STAR))
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_stolen_traffic(
        clients: &HashMap<ClientId, Client>,
        traffic: StolenTraffic,
        subscription: &PortSubscription,
        ongoing: &mut JoinSet<RedirectedHttp>,
    ) {
        let protocol_version_req = match &traffic {
            StolenTraffic::Tcp { conn, .. } => Self::protocol_version_req_tcp(subscription, conn),
            StolenTraffic::Http(http) => Self::protocol_version_req_http(subscription, http),
        };

        let (filters, mut http) = match (subscription, traffic) {
            (PortSubscription::Filtered(filters), StolenTraffic::Http(http)) => (filters, http),

            (
                PortSubscription::Filtered(..),
                StolenTraffic::Tcp {
                    conn,
                    join_handle_tx,
                    shutdown,
                },
            ) => {
                join_handle_tx.
                    send(conn.pass_through(shutdown))
                    .expect("RedirectorTask dropped oneshot rx for receiving JoinHandle to IO task for TCP connection");
                return;
            }

            (
                PortSubscription::Unfiltered(client_id),
                StolenTraffic::Tcp {
                    conn,
                    join_handle_tx,
                    shutdown,
                },
            ) => {
                let Some(client) = clients.get(client_id) else {
                    tracing::error!(
                        client_id,
                        "TcpStealerTask failed to find a connected client for a stolen TCP connection, \
                        the connection will be passed through to its original destination. \
                        This is a bug in the agent, please report it.",
                    );
                    join_handle_tx
                        .send(conn.pass_through(shutdown))
                        .expect("RedirectorTask dropped oneshot rx for receiving JoinHandle to IO task for TCP connection");
                    return;
                };

                let message = if client.protocol_version.matches(&protocol_version_req) {
                    let (steal_handle, join_handle) = conn.steal(shutdown);
                    join_handle_tx
                        .send(join_handle)
                        .expect("RedirectorTask dropped oneshot rx for receiving JoinHandle to IO task for TCP connection");

                    StealerMessage::StolenTcp(steal_handle)
                } else {
                    join_handle_tx
                        .send(conn.pass_through(shutdown))
                        .expect("RedirectorTask dropped oneshot rx for receiving JoinHandle to IO task for TCP connection");

                    StealerMessage::Log(LogMessage::error(format!(
                        "A TCP connection was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req,
                    )))
                };

                let _ = client.message_tx.send(message).await;
                return;
            }

            (PortSubscription::Unfiltered(client_id), StolenTraffic::Http(http)) => {
                let Some(client) = clients.get(client_id) else {
                    tracing::error!(
                        client_id,
                        "TcpStealerTask failed to find a connected client for a stolen HTTP request, \
                        the request will be passed through to its original destination. \
                        This is a bug in the agent, please report it.",
                    );
                    http.pass_through();
                    return;
                };

                let message = if client.protocol_version.matches(&protocol_version_req) {
                    StealerMessage::StolenHttp(http.steal())
                } else {
                    http.pass_through();
                    StealerMessage::Log(LogMessage::error(format!(
                        "An HTTP request was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req,
                    )))
                };

                let _ = client.message_tx.send(message).await;
                return;
            }
        };

        if filters.values().any(HttpFilter::needs_body) {
            ongoing.spawn(async move {
                if let Err(error) = http.buffer_body().await {
                    tracing::debug!(?error, "failed to buffer request body");
                };
                http
            });
        } else {
            Self::finish_stealing(clients, filters, http, protocol_version_req).await
        }
    }

    async fn finish_stealing(
        clients: &HashMap<ClientId, Client>,
        filters: &HashMap<ClientId, HttpFilter>,
        mut http: RedirectedHttp,
        protocol_version_req: Cow<'static, semver::VersionReq>,
    ) {
        let mut send_to = None; // the client that will receive the request
        let mut preempted = vec![]; // other clients that could receive the request as well
        let mut blocked_on_protocol = vec![]; // clients that cannot receive the request due to their protocol version

        let (parts, body_reader) = http.parts_and_body();

        for (client_id, filter) in filters {
            if filter.matches(parts, body_reader).not() {
                continue;
            }

            let Some(client) = clients.get(client_id) else {
                tracing::error!(
                    client_id,
                    "TcpStealerTask failed to find a connected client for a stolen HTTP request. \
                        This is a bug in the agent, please report it.",
                );
                continue;
            };

            if client.protocol_version.matches(&protocol_version_req).not() {
                blocked_on_protocol.push(client);
            } else if send_to.is_none() {
                send_to = Some(client);
            } else {
                preempted.push(client);
            }
        }

        for client in preempted {
            let _ = client
                .message_tx
                .send(StealerMessage::Log(LogMessage::warn(format!(
                    "An HTTP request was stolen by another user. \
                    METHOD=({}) URI=({}), HEADERS=({:?}) PORT=({})",
                    http.parts().method,
                    http.parts().uri,
                    http.parts().headers,
                    http.info().original_destination.port(),
                ))))
                .await;
        }

        for client in blocked_on_protocol {
            let _ = client.message_tx.send(StealerMessage::Log(LogMessage::error(format!(
                    "An HTTP request was not stolen due to mirrord-protocol version requirement: {}. \
                    METHOD=({}) URI=({}), HEADERS=({:?}) PORT=({})",
                    protocol_version_req,
                    http.parts().method,
                    http.parts().uri,
                    http.parts().headers,
                    http.info().original_destination.port(),
            )))).await;
        }

        if let Some(client) = send_to {
            let _ = client
                .message_tx
                .send(StealerMessage::StolenHttp(http.steal()))
                .await;
        } else {
            http.pass_through();
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::ERROR))]
    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), RedirectorTaskError> {
        match command.command {
            Command::NewClient(message_tx, protocol_version) => {
                let Entry::Vacant(e) = self.clients.entry(command.client_id) else {
                    unreachable!("client id already exists");
                };

                self.disconnected_clients.push(ChannelClosedFuture::new(
                    message_tx.clone(),
                    command.client_id,
                ));

                e.insert(Client {
                    message_tx,
                    protocol_version,
                });
            }

            Command::PortSubscribe(port, filter) => {
                let Some(client) = self.clients.get(&command.client_id) else {
                    // The client disconnected after sending the message.
                    return Ok(());
                };

                self.subscriptions
                    .add(command.client_id, port, filter)
                    .await?;

                let _ = client
                    .message_tx
                    .send(StealerMessage::PortSubscribed(port))
                    .await;
            }

            Command::PortUnsubscribe(port) => {
                self.subscriptions.remove(command.client_id, port);
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_client_disconnected(&mut self, client_id: ClientId) {
        self.clients.remove(&client_id);
        self.subscriptions.remove_all(client_id);
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_buffered_http(&mut self, http: RedirectedHttp) {
        let Some(subscription) = self
            .subscriptions
            .get(http.info().original_destination.port())
        else {
            tracing::warn!(
                ?http,
                "Finished buffering HTTP body for a port that is no longer stolen, dropping",
            );
            http.pass_through();
            return;
        };

        let PortSubscription::Filtered(filters) = subscription else {
            tracing::error!(
                ?http,
                "handle_buffered_http called for a request on an unfiltered port. This is a bug in the agent, please report it.",
            );
            http.pass_through();
            return;
        };

        let protocol_version_req = Self::protocol_version_req_http(subscription, &http);
        Self::finish_stealing(&self.clients, filters, http, protocol_version_req).await;
    }
}

impl fmt::Debug for TcpStealerTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStealerTask")
            .field("subscriptions", &self.subscriptions)
            .field("clients", &self.clients)
            .finish()
    }
}

#[derive(Debug)]
struct Client {
    message_tx: mpsc::Sender<StealerMessage>,
    protocol_version: ClientProtocolVersion,
}
