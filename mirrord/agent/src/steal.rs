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
pub(crate) mod tls;

pub(crate) use api::TcpStealApi;

enum StealerMessage {
    Log(LogMessage),
    PortSubscribed(u16),
    StolenTcp(StolenTcp),
    StolenHttp(StolenHttp),
}

#[derive(Debug)]
enum Command {
    NewClient {
        message_tx: mpsc::Sender<StealerMessage>,
        protocol_version: SharedProtocolVersion,
    },
    PortSubscribe {
        port: u16,
        filter: Option<HttpFilter>,
    },
    PortUnsubscribe(u16),
}

#[derive(Debug)]
pub struct StealerCommand {
    client_id: ClientId,
    command: Command,
}

pub struct TcpStealerTask {
    subscriptions: PortSubscriptions,
    command_rx: mpsc::Receiver<StealerCommand>,
    clients: HashMap<ClientId, Client>,
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

    fn protocol_version_req(traffic: &StolenTraffic) -> Cow<'static, semver::VersionReq> {
        match traffic {
            StolenTraffic::Tcp(tcp) => tcp
                .info()
                .tls_connector
                .is_some()
                .then_some(&*NEW_CONNECTION_V2_VERSION)
                .map(Cow::Borrowed)
                .unwrap_or(Cow::Owned(semver::VersionReq::STAR)),

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
        let protocol_version_req = Self::protocol_version_req(&traffic);

        let (filters, mut http) = match (subscription, traffic) {
            (PortSubscription::Unfiltered(client_id), StolenTraffic::Tcp(tcp)) => {
                let client = clients.get(client_id).expect("client not found");

                let message = if client.protocol_version.matches(&protocol_version_req).not() {
                    tcp.pass_through();
                    StealerMessage::Log(LogMessage::warn(format!(
                        "A TCP connection was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req,
                    )))
                } else {
                    StealerMessage::StolenTcp(tcp.steal())
                };

                let _ = client.message_tx.send(message).await;

                return;
            }

            (PortSubscription::Unfiltered(client_id), StolenTraffic::Http(http)) => {
                let client = clients.get(client_id).expect("client not found");

                let message = if client.protocol_version.matches(&protocol_version_req).not() {
                    http.pass_through();
                    StealerMessage::Log(LogMessage::warn(format!(
                        "A TCP connection was not stolen due to mirrord-protocol version requirement: {}",
                        protocol_version_req,
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

            (PortSubscription::Filtered(filters), StolenTraffic::Http(http)) => (filters, http),
        };

        let mut send_to = None;
        let mut preempted = vec![];
        let mut blocked_on_protocol = vec![];
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
                    protocol_version_req,
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
                self.subscriptions
                    .subscribe(port, command.client_id, filter)
                    .await?;
                let _ = self
                    .clients
                    .get(&command.client_id)
                    .expect("client not found")
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
