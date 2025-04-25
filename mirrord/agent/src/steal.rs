use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    ops::Not,
};

use futures::{stream::FuturesUnordered, StreamExt};
use hyper::http::header::UPGRADE;
use mirrord_protocol::{
    tcp::{
        HTTP_CHUNKED_REQUEST_V2_VERSION, HTTP_FILTERED_UPGRADE_VERSION, NEW_CONNECTION_V2_VERSION,
    },
    LogMessage,
};
use subscriptions::{PortSubscription, PortSubscriptions};
use tokio::sync::mpsc;

use crate::{
    http::HttpFilter,
    incoming::{RedirectorTaskError, StealHandle, StolenHttp, StolenTcp, StolenTraffic},
    util::{protocol_version::SharedProtocolVersion, ChannelClosedFuture, ClientId},
};

mod api;
mod subscriptions;
mod test;

pub(crate) use api::TcpStealApi;

/// Sent from the [`TcpStealerTask`] to the [`TcpStealApi`].
enum StealerMessage {
    Log(LogMessage),
    PortSubscribed(u16),
    StolenTcp(StolenTcp),
    StolenHttp(StolenHttp),
}

#[derive(Debug)]
enum Command {
    /// Sent when a new instance of [`TcpStealApi`] is created.
    NewClient {
        /// [`TcpStealerTask`] will use this channel to asynchronously send messages to the
        /// [`TcpStealApi`].
        message_tx: mpsc::Sender<StealerMessage>,
        /// [`mirrord_protocol`] version of the client.
        ///
        /// [`TcpStealerTask`] will use this when distributing the stolen traffic.
        protocol_version: SharedProtocolVersion,
    },
    PortSubscribe {
        port: u16,
        filter: Option<HttpFilter>,
    },
    PortUnsubscribe(u16),
}

/// Command sent from the [`TcpStealApi`] to the [`TcpStealerTask`].
#[derive(Debug)]
pub struct StealerCommand {
    client_id: ClientId,
    command: Command,
}

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
    pub fn new(handle: StealHandle, command_rx: mpsc::Receiver<StealerCommand>) -> Self {
        Self {
            subscriptions: PortSubscriptions::new(handle),
            command_rx,
            clients: Default::default(),
            disconnected_clients: Default::default(),
        }
    }

    pub async fn run(mut self) -> Result<(), RedirectorTaskError> {
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
            }
        }

        Ok(())
    }

    /// Returns a [`semver::VersionReq`] for the given subscription and stolen traffic.
    ///
    /// Client's [`mirrord_protocol`] version must match the requirement
    /// for the client to receive this traffic. Otherwise, the client will not be able to handle
    /// the traffic.
    fn protocol_version_req(
        subscription: &PortSubscription,
        traffic: &StolenTraffic,
    ) -> Cow<'static, semver::VersionReq> {
        match traffic {
            StolenTraffic::Tcp(tcp) => tcp
                .info()
                .tls_connector
                .is_some()
                .then_some(&*NEW_CONNECTION_V2_VERSION)
                .map(Cow::Borrowed)
                .unwrap_or(Cow::Owned(semver::VersionReq::STAR)),

            StolenTraffic::Http(http) => matches!(subscription, PortSubscription::Unfiltered(..))
                .then_some(&*NEW_CONNECTION_V2_VERSION)
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
                let client = clients.get(client_id).expect("client not found");

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
                let client = clients.get(client_id).expect("client not found");

                let message = if client.protocol_version.matches(&protocol_version_req) {
                    StealerMessage::StolenHttp(http.steal())
                } else {
                    http.pass_through();
                    StealerMessage::Log(LogMessage::error(format!(
                        "A TCP connection was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req,
                    )))
                };

                let _ = client.message_tx.send(message).await;
                return;
            }
        };

        let mut send_to = None; // the client that will receive the request
        let mut preempted = vec![]; // other clients that could receive the request as well
        let mut blocked_on_protocol = vec![]; // clients that cannot receive the request due to their protocol version
        filters
            .iter()
            .filter(|(_, filter)| filter.matches(http.parts_mut()))
            .map(|(client_id, _)| clients.get(client_id).expect("client not found"))
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

    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), RedirectorTaskError> {
        match command.command {
            Command::NewClient {
                message_tx,
                protocol_version,
            } => {
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

            Command::PortSubscribe { port, filter } => {
                let Some(client) = self.clients.get(&command.client_id) else {
                    // The client disconnected after sending the message.
                    return Ok(());
                };

                self.subscriptions
                    .subscribe(port, command.client_id, filter)
                    .await?;

                let _ = client
                    .message_tx
                    .send(StealerMessage::PortSubscribed(port))
                    .await;
            }

            Command::PortUnsubscribe(port) => {
                self.subscriptions.unsubscribe(port, command.client_id);
            }
        }

        Ok(())
    }

    fn handle_client_disconnected(&mut self, client_id: ClientId) {
        self.clients.remove(&client_id);
        self.subscriptions.unsubscribe_all(client_id);
    }
}

struct Client {
    message_tx: mpsc::Sender<StealerMessage>,
    protocol_version: SharedProtocolVersion,
}
