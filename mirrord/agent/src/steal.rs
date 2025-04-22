use std::{
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
use semver::VersionReq;
use tokio::sync::mpsc;

use crate::{
    http::HttpFilter,
    incoming::{RedirectorTaskError, StealHandle, StolenHttp, StolenTcp, StolenTraffic},
    util::{ChannelClosedFuture, ClientId},
};

mod api;
pub(crate) mod tls;

pub(crate) use api::TcpStealerApi;
pub(crate) use tls::StealTlsHandlerStore;

pub enum StealerMessage {
    Log(LogMessage),
    PortSubscribed(u16),
    StolenTcp(StolenTcp),
    StolenHttp(StolenHttp),
}

#[derive(Debug)]
enum Command {
    NewClient(mpsc::Sender<StealerMessage>),
    PortSubscribe {
        port: u16,
        filter: Option<HttpFilter>,
    },
    PortUnsubscribe(u16),
    SwitchProtocolVersion(semver::Version),
}

#[derive(Debug)]
pub struct StealerCommand {
    client_id: ClientId,
    command: Command,
}

pub struct TcpStealerTask {
    handle: StealHandle,
    command_rx: mpsc::Receiver<StealerCommand>,
    clients: HashMap<ClientId, Client>,
    disconnected_clients: FuturesUnordered<ChannelClosedFuture>,
    subscriptions: HashMap<u16, PortSubscription>,
}

impl TcpStealerTask {
    pub fn new(handle: StealHandle, command_rx: mpsc::Receiver<StealerCommand>) -> Self {
        Self {
            handle,
            command_rx,
            clients: Default::default(),
            disconnected_clients: Default::default(),
            subscriptions: Default::default(),
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

                Some(result) = self.handle.next() => {
                    let traffic = result?;
                    self.handle_stolen_traffic(traffic).await;
                }

                Some(client_id) = self.disconnected_clients.next() => {
                    self.handle_client_disconnected(client_id);
                }
            }
        }

        Ok(())
    }

    fn protocol_version_req(traffic: &StolenTraffic) -> Option<&'static VersionReq> {
        match traffic {
            StolenTraffic::Tcp(tcp) => tcp
                .info()
                .tls_connector
                .is_some()
                .then_some(&*NEW_CONNECTION_V2_VERSION),
            StolenTraffic::Http(http) => http
                .info()
                .tls_connector
                .is_some()
                .then_some(&*HTTP_CHUNKED_REQUEST_V2_VERSION)
                .or_else(|| {
                    http.parts()
                        .headers
                        .contains_key(UPGRADE)
                        .then_some(&*HTTP_FILTERED_UPGRADE_VERSION)
                }),
        }
    }

    async fn handle_stolen_traffic(&mut self, traffic: StolenTraffic) {
        let subscription = self
            .subscriptions
            .get(&traffic.destination_port())
            .expect("subscription not found");

        let protocol_version_req = Self::protocol_version_req(&traffic);

        let (clients, mut http) = match (subscription, traffic) {
            (PortSubscription::Unfiltered(client_id), StolenTraffic::Tcp(tcp)) => {
                let client = self.clients.get(client_id).expect("client not found");

                let blocked = protocol_version_req.is_some_and(|req| {
                    client
                        .protocol_version
                        .as_ref()
                        .is_none_or(|version| req.matches(version).not())
                });

                let message = if blocked {
                    tcp.pass_through();
                    StealerMessage::Log(LogMessage::warn(format!(
                        "A TCP connection was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req.unwrap_or(&VersionReq::STAR),
                    )))
                } else {
                    StealerMessage::StolenTcp(tcp.steal())
                };

                let _ = client.message_tx.send(message).await;

                return;
            }

            (PortSubscription::Unfiltered(client_id), StolenTraffic::Http(http)) => {
                let client = self.clients.get(client_id).expect("client not found");

                let blocked = protocol_version_req.is_some_and(|req| {
                    client
                        .protocol_version
                        .as_ref()
                        .is_none_or(|version| req.matches(version).not())
                });

                let message = if blocked {
                    http.pass_through();
                    StealerMessage::Log(LogMessage::warn(format!(
                        "A TCP connection was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req.unwrap_or(&VersionReq::STAR),
                    )))
                } else {
                    StealerMessage::StolenHttp(http.steal())
                };

                let _ = client.message_tx.send(message).await;

                return;
            }

            (PortSubscription::Filtered(..), StolenTraffic::Tcp(tcp)) => {
                tcp.pass_through();
                return;
            }

            (PortSubscription::Filtered(clients), StolenTraffic::Http(http)) => (clients, http),
        };

        let mut send_to = None;
        let mut preempted = vec![];
        let mut blocked_on_protocol = vec![];
        clients
            .iter()
            .filter(|(_, filter)| filter.matches(http.parts_mut()))
            .map(|(client_id, _)| self.clients.get(client_id).expect("client not found"))
            .for_each(|client| {
                let blocked = protocol_version_req.is_some_and(|req| {
                    client
                        .protocol_version
                        .as_ref()
                        .is_none_or(|version| req.matches(version).not())
                });
                if blocked {
                    blocked_on_protocol.push(client);
                } else if send_to.is_none() {
                    send_to = Some(client);
                } else {
                    preempted.push(client);
                }
            });

        for client in preempted {
            let _ = client.message_tx.send(StealerMessage::Log(LogMessage::warn(format!(
                    "An HTTP request was stolen by another user. URI=({}), HEADERS=({:?}) PORT=({})",
                    http.parts().uri,
                    http.parts().headers,
                    http.info().original_destination.port(),
                )),
            ))
            .await;
        }

        for client in blocked_on_protocol {
            let _ = client.message_tx.send(StealerMessage::Log(LogMessage::warn(format!(
                    "An HTTP request was not stolen due to mirrord-protocol version requirement: {}. \
                    URI=({}), HEADERS=({:?}) PORT=({})",
                    protocol_version_req.unwrap_or(&VersionReq::STAR),
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
            Command::NewClient(message_tx) => {
                let Entry::Vacant(e) = self.clients.entry(command.client_id) else {
                    unreachable!("client id already exists");
                };

                self.disconnected_clients.push(ChannelClosedFuture::new(
                    message_tx.clone(),
                    command.client_id,
                ));

                e.insert(Client {
                    message_tx,
                    protocol_version: None,
                });
            }

            Command::PortSubscribe { port, filter } => {
                match self.subscriptions.entry(port) {
                    Entry::Occupied(mut e) => {
                        e.get_mut().extend_or_replace(command.client_id, filter);
                    }
                    Entry::Vacant(e) => {
                        self.handle.steal(port).await?;
                        let subscription = match filter {
                            Some(filter) => PortSubscription::Filtered(HashMap::from([(
                                command.client_id,
                                filter,
                            )])),
                            None => PortSubscription::Unfiltered(command.client_id),
                        };
                        e.insert(subscription);
                    }
                }

                let _ = self
                    .clients
                    .get(&command.client_id)
                    .expect("client not found")
                    .message_tx
                    .send(StealerMessage::PortSubscribed(port))
                    .await;
            }

            Command::PortUnsubscribe(port) => {
                let Entry::Occupied(mut e) = self.subscriptions.entry(port) else {
                    return Ok(());
                };

                if e.get_mut().remove_if_exists(command.client_id).not() {
                    self.handle.stop_steal(port);
                    e.remove();
                }
            }

            Command::SwitchProtocolVersion(version) => {
                self.clients
                    .get_mut(&command.client_id)
                    .expect("client not found")
                    .protocol_version
                    .replace(version);
            }
        }

        Ok(())
    }

    fn handle_client_disconnected(&mut self, client_id: ClientId) {
        self.clients.remove(&client_id);
        self.subscriptions.retain(|port, subscription| {
            let retain = subscription.remove_if_exists(client_id);
            if retain.not() {
                self.handle.stop_steal(*port);
            }
            retain
        });
    }
}

struct Client {
    message_tx: mpsc::Sender<StealerMessage>,
    protocol_version: Option<semver::Version>,
}

enum PortSubscription {
    Unfiltered(ClientId),
    Filtered(HashMap<ClientId, HttpFilter>),
}

impl PortSubscription {
    fn remove_if_exists(&mut self, client_id: ClientId) -> bool {
        match self {
            Self::Unfiltered(client) => *client != client_id,
            Self::Filtered(clients) => {
                clients.remove(&client_id);
                clients.is_empty().not()
            }
        }
    }

    fn extend_or_replace(&mut self, client_id: ClientId, filter: Option<HttpFilter>) {
        match (self, filter) {
            (this, None) => *this = Self::Unfiltered(client_id),

            (this @ Self::Unfiltered(..), Some(filter)) => {
                *this = Self::Filtered(HashMap::from([(client_id, filter)]));
            }

            (Self::Filtered(clients), Some(filter)) => {
                clients.insert(client_id, filter);
            }
        }
    }
}
