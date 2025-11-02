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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Level};

use super::{
    Command, StealerCommand, StealerMessage,
    subscriptions::{PortSubscription, PortSubscriptions},
};
use crate::{
    http::filter::HttpFilter,
    incoming::{BufferBodyError, RedirectedHttp, RedirectorTaskError, StealHandle, StolenTraffic},
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
}

impl TcpStealerTask {
    pub fn new(command_rx: mpsc::Receiver<StealerCommand>, handle: StealHandle) -> Self {
        Self {
            subscriptions: PortSubscriptions::new(handle),
            command_rx,
            clients: Default::default(),
            disconnected_clients: Default::default(),
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
                    Self::handle_stolen_traffic(&self.clients, traffic, subscription).await;
                }

                Some(client_id) = self.disconnected_clients.next() => {
                    self.handle_client_disconnected(client_id);
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
    fn protocol_version_req(
        subscription: &PortSubscription,
        traffic: &StolenTraffic,
    ) -> Cow<'static, semver::VersionReq> {
        match traffic {
            StolenTraffic::Tcp(tcp) => tcp
                .info()
                .tls_connector
                .is_some()
                .then_some(&*MODE_AGNOSTIC_HTTP_REQUESTS)
                .map(Cow::Borrowed)
                .unwrap_or(Cow::Owned(semver::VersionReq::STAR)),

            StolenTraffic::Http(http) => matches!(subscription, PortSubscription::Unfiltered(..))
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
                .unwrap_or(Cow::Owned(semver::VersionReq::STAR)),
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_stolen_traffic(
        clients: &HashMap<ClientId, Client>,
        traffic: StolenTraffic,
        subscription: &PortSubscription,
    ) {
        let protocol_version_req = Self::protocol_version_req(subscription, &traffic);

        let (filters, mut http) = match (subscription, traffic) {
            (PortSubscription::Filtered(filters), StolenTraffic::Http(http)) => (filters, http),

            (PortSubscription::Filtered(..), StolenTraffic::Tcp(tcp)) => {
                tcp.pass_through();
                return;
            }

            (PortSubscription::Unfiltered(client_id), StolenTraffic::Tcp(tcp)) => {
                let Some(client) = clients.get(client_id) else {
                    tracing::error!(
                        client_id,
                        "TcpStealerTask failed to find a connected client for a stolen TCP connection, \
                        the connection will be passed through to its original destination. \
                        This is a bug in the agent, please report it.",
                    );
                    tcp.pass_through();
                    return;
                };

                let message = if client.protocol_version.matches(&protocol_version_req) {
                    StealerMessage::StolenTcp(tcp.steal())
                } else {
                    tcp.pass_through();
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

        // Optimize the fast path, no need to spawn a new task and clone clients and filters.
        if filters.values().all(|f| f.needs_body().not()) {
            tracing::trace!("have no body filters, using fast path");
            Self::finish_handling_stolen_traffic(
                filters,
                clients,
                http,
                None,
                protocol_version_req,
            )
            .await;
        } else {
            let span = tracing::trace_span!("have body filters, spawning new task to buffer request body");
            let clients = clients.clone();
            let filters = filters.clone();
            tokio::spawn(
                async move {
                    let result = http.buffer_body().await;
                    let body = match result {
                        Ok(()) => Some(http.body_head()),
                        Err(e) => match e {
                            BufferBodyError::Hyper(err) => {
                                tracing::warn!(?err, "error while receiving http body for filter");
                                None
                            }
                            BufferBodyError::UnexpectedEOB
                            | BufferBodyError::BodyTooBig
                            | BufferBodyError::Timeout(_) => None,
                        },
                    };
                    Self::finish_handling_stolen_traffic(
                        &filters,
                        &clients,
                        http,
                        body.as_deref(),
                        protocol_version_req,
                    )
                    .await;
                }
                .instrument(span),
            );
        };
    }

    async fn finish_handling_stolen_traffic(
        filters: &HashMap<ClientId, HttpFilter>,
        clients: &HashMap<ClientId, Client>,
        mut http: RedirectedHttp,
        body: Option<&[u8]>,
        protocol_version_req: Cow<'_, semver::VersionReq>,
    ) {
        let mut send_to = None; // the client that will receive the request
        let mut preempted = vec![]; // other clients that could receive the request as well
        let mut blocked_on_protocol = vec![]; // clients that cannot receive the request due to their protocol version

        filters
            .iter()
				.filter(|(_, filter)| filter.matches(http.parts_mut(), body.as_deref()))
            .filter_map(|(client_id, _)| {
                clients.get(client_id).or_else(|| {
                    tracing::error!(
                        client_id,
                        "TcpStealerTask failed to find a connected client for a stolen HTTP request. \
                        This is a bug in the agent, please report it.",
                    );
                    None
                })
            })
            .for_each(|client| {
                if client.protocol_version.matches(&protocol_version_req).not() {
                    blocked_on_protocol.push(client);
                } else if send_to.is_none() {
                    send_to = Some(client);
                } else {
                    preempted.push(client);
                }
            });

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
}

impl fmt::Debug for TcpStealerTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStealerTask")
            .field("subscriptions", &self.subscriptions)
            .field("clients", &self.clients)
            .finish()
    }
}

#[derive(Debug, Clone)]
struct Client {
    message_tx: mpsc::Sender<StealerMessage>,
    protocol_version: ClientProtocolVersion,
}
