use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::Entry},
    fmt,
    io::Read,
    ops::Not,
};

use futures::{StreamExt, stream::FuturesUnordered};
use http::header::UPGRADE;
use hyper::http::request::Parts;
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
    http::filter::{HttpFilter, MatchResult, RequestBody},
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
    ongoing_requests: JoinSet<(RedirectedHttp, SettledFilters)>,
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
                        Ok((http, settled)) => {
                            self.handle_buffered_http(http, settled).await;
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
        ongoing: &mut JoinSet<(RedirectedHttp, SettledFilters)>,
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

        let (parts, body) = http.parts_and_body();
        match Self::decide(filters, SettledFilters::new(), parts, body).await {
            StealDecision::BufferBody(settled) => {
                ongoing.spawn(async move {
                    if let Err(error) = http.buffer_body().await {
                        tracing::debug!(?error, "failed to buffer request body");
                    };
                    (http, settled)
                });
            }
            StealDecision::Matched(matched) => {
                Self::finish_stealing(clients, matched, http, protocol_version_req).await
            }
        }
    }

    /// Runs every client's filter over a stolen request, skipping those already in `settled`.
    ///
    /// The body is buffered only when a filter asks for it, which happens when the headers alone
    /// cannot settle that filter. The filters that did answer keep their answer, so buffering
    /// costs no second round of `header_jq` evaluations. After a buffering attempt the body is
    /// complete, given up on, or broken for good, so the remaining filters always come back with a
    /// final answer. A filter that could not be run does not steal: the request goes where it
    /// would have gone without that client.
    ///
    /// `filters` is read afresh on each call: a client that unsubscribed while the body was
    /// buffering does not get the request, and one that subscribed meanwhile gets its filter run.
    async fn decide<T: Read + Copy>(
        filters: &HashMap<ClientId, HttpFilter>,
        mut settled: SettledFilters,
        parts: &mut Parts,
        body: RequestBody<T>,
    ) -> StealDecision {
        let mut needs_body = false;

        for (client_id, filter) in filters {
            if settled.contains_key(client_id) {
                continue;
            }

            match filter.matches(parts, body).await {
                MatchResult::Match => {
                    settled.insert(*client_id, true);
                }
                MatchResult::NoMatch | MatchResult::Undecided => {
                    settled.insert(*client_id, false);
                }
                MatchResult::NeedsBody => needs_body = true,
            }
        }

        if needs_body {
            return StealDecision::BufferBody(settled);
        }

        StealDecision::Matched(
            filters
                .keys()
                .filter(|client_id| settled.get(client_id).copied().unwrap_or(false))
                .copied()
                .collect(),
        )
    }

    async fn finish_stealing(
        clients: &HashMap<ClientId, Client>,
        matched: Vec<ClientId>,
        http: RedirectedHttp,
        protocol_version_req: Cow<'static, semver::VersionReq>,
    ) {
        let mut send_to = None; // the client that will receive the request
        let mut preempted = vec![]; // other clients that could receive the request as well
        let mut blocked_on_protocol = vec![]; // clients that cannot receive the request due to their protocol version

        for client_id in &matched {
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
    async fn handle_buffered_http(&mut self, mut http: RedirectedHttp, settled: SettledFilters) {
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
        let (parts, body) = http.parts_and_body();
        match Self::decide(filters, settled, parts, body).await {
            StealDecision::Matched(matched) => {
                Self::finish_stealing(&self.clients, matched, http, protocol_version_req).await
            }
            StealDecision::BufferBody(..) => {
                tracing::error!(
                    ?http,
                    "A filter asked to buffer a request body that was already buffered or given up on. \
                    This is a bug in the agent, please report it.",
                );
                http.pass_through();
            }
        }
    }
}

/// What each client's filter answered about one request, `true` meaning it steals. Filters that
/// still need the body are missing.
type SettledFilters = HashMap<ClientId, bool>;

/// Outcome of running the clients' filters over a stolen request.
#[derive(Debug, PartialEq, Eq)]
enum StealDecision {
    /// Clients whose filter matches, in filter map order. The first connected one with a good
    /// enough protocol version gets the request, the rest are told another user took it.
    Matched(Vec<ClientId>),
    /// Some filter can only decide once the body is buffered. Carries the answers of the others.
    BufferBody(SettledFilters),
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

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use hyper::Request;
    use mirrord_protocol::tcp::{self, Filter, JsonPathQuery};

    use super::{SettledFilters, StealDecision, TcpStealerTask};
    use crate::http::filter::{HttpFilter, RequestBody};

    fn tenant_header_filter() -> HttpFilter {
        let filter = tcp::HttpFilter::Header(Filter::new("x-tenant: preview".to_owned()).unwrap());
        HttpFilter::try_from(&filter).unwrap()
    }

    fn tenant_body_filter() -> HttpFilter {
        let filter = tcp::HttpFilter::Body(tcp::HttpBodyFilter::Json {
            query: JsonPathQuery::new("$.tenant".to_owned()).unwrap(),
            matches: Filter::new("preview".to_owned()).unwrap(),
        });
        HttpFilter::try_from(&filter).unwrap()
    }

    fn sorted(decision: StealDecision) -> Vec<u32> {
        let StealDecision::Matched(mut matched) = decision else {
            panic!("expected a final decision, got {decision:?}");
        };
        matched.sort();
        matched
    }

    /// On a port with header and body filters, a POST goes through the filters twice: once on the
    /// head and once after buffering. Every `header_jq` evaluation costs a blocking task and up to
    /// the jq time limit, so the second pass runs only the filters that asked for the body. The
    /// answers from the first pass are kept, here including a `Match` next to a `NeedsBody`.
    #[tokio::test]
    async fn only_filters_that_asked_for_the_body_run_after_buffering() {
        let filters = HashMap::from([
            (1, tenant_header_filter()),
            (2, tenant_body_filter()),
            (3, tenant_header_filter()),
        ]);
        let mut parts = Request::builder()
            .uri("/orders")
            .header("x-tenant", "preview")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let decision = TcpStealerTask::decide(
            &filters,
            SettledFilters::new(),
            &mut parts,
            RequestBody::<&[u8]>::Pending,
        )
        .await;
        let StealDecision::BufferBody(mut settled) = decision else {
            panic!("expected a request to buffer the body, got {decision:?}");
        };
        assert_eq!(settled, HashMap::from([(1, true), (3, true)]));

        // Flip the kept answer of client 3: a filter that ran again would answer `true`.
        settled.insert(3, false);
        let decision = TcpStealerTask::decide(
            &filters,
            settled,
            &mut parts,
            RequestBody::Complete(br#"{"tenant":"preview"}"#.as_slice()),
        )
        .await;
        assert_eq!(sorted(decision), [1, 2]);
    }

    /// Buffering takes a while and clients come and go. A client that left does not get the
    /// request even though its filter matched on the head, and one that subscribed in the
    /// meantime has its filter run.
    #[tokio::test]
    async fn clients_changing_during_buffering_are_respected() {
        let filters = HashMap::from([(2, tenant_header_filter())]);
        let mut parts = Request::builder()
            .uri("/orders")
            .header("x-tenant", "preview")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let decision = TcpStealerTask::decide(
            &filters,
            HashMap::from([(1, true)]),
            &mut parts,
            RequestBody::Complete(b"".as_slice()),
        )
        .await;
        assert_eq!(sorted(decision), [2]);
    }
}
