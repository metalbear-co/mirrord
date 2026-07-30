//! DNS filtering: answering DNS traffic that the application sends straight to a DNS server.
//!
//! mirrord normally resolves names by hooking `getaddrinfo` and friends in the layer. Runtimes
//! that bundle their own resolver never call those functions, so their lookups are invisible to
//! the hooks and arrive here instead, as ordinary outgoing traffic to port [`DNS_PORT`].
//!
//! Tunneling that traffic to the target does not help: the resolver picked its DNS server from
//! the *local* machine's configuration, and that address usually means nothing inside the
//! cluster. So instead of forwarding the bytes, [`DnsFiltering`] parses the query, resolves it
//! with the same remote lookup that backs `getaddrinfo`, and writes an answer back to the
//! application itself.
//!
//! Queries we have no way to answer that way, anything that is not an `A` or `AAAA` lookup,
//! make the connection fall back to [`DnsSocketState::Tunneled`], where it behaves like any
//! other outgoing connection.
//!
//! Not to be confused with `feature.network.dns.filter`, which decides *which names* resolve
//! remotely. This decides *how* directly-sent queries are served.

use std::{
    collections::{HashMap, VecDeque},
    io,
    net::IpAddr,
    ops::Not,
    sync::Arc,
};

use bytes::{Buf, Bytes, BytesMut};
use hickory_proto::{
    op::{Message, MessageType, OpCode, ResponseCode},
    rr::{
        DNSClass, RData, Record, RecordType,
        rdata::{A, AAAA},
    },
};
use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
    OutgoingResponse, ProxyToLayerMessage,
};
use mirrord_protocol::{
    ConnectionId, RemoteResult,
    dns::{AddressFamily, DnsLookup, GetAddrInfoRequestV2, SockType},
    outgoing::{DaemonConnect, SocketAddress},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::Level;

use super::{
    InterceptorId, OutgoingProxy,
    interceptor::{Interceptor, InterceptorCommand, WRITE_BUDGET_BYTES},
    net_protocol_ext::NetProtocolExt,
};
use crate::{
    background_tasks::{BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate},
    main_tasks::{DnsFilteringLookup, DnsFilteringLookupResult, ToLayer},
};

/// Port on which outgoing traffic is assumed to be DNS.
pub const DNS_PORT: u16 = 53;

/// Identifies one intercepted query while its remote lookup is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DnsQueryId(pub u64);

/// TTL, in seconds, of the records we synthesize.
///
/// Deliberately short. The answers describe pods, which come and go, and the resolver caching
/// them has no way to learn that an address went stale. Making it come back to us often is
/// cheap, since answering costs one round trip to the agent.
const ANSWER_TTL: u32 = 30;

/// Largest response we may send over UDP when the query carries no EDNS OPT record.
///
/// [RFC 1035 4.2.1](https://www.rfc-editor.org/rfc/rfc1035#section-4.2.1). Anything longer has
/// to be truncated, so that the resolver retries over TCP.
const MAX_UDP_PAYLOAD: usize = 512;

/// State of the DNS filtering feature, owned by the [`OutgoingProxy`].
pub(super) struct DnsFiltering {
    /// Whether traffic to [`DNS_PORT`] is answered here instead of being tunneled.
    enabled: bool,
    /// Tasks owning the local sockets of intercepted DNS connections.
    ///
    /// Kept apart from the [`OutgoingProxy`]'s own registry because these ids are allocated
    /// here and mean nothing to the agent, while the ids there are agent connection ids.
    tasks: Option<BackgroundTasks<InterceptorId, (Bytes, OwnedSemaphorePermit), io::Error>>,
    /// State of each intercepted DNS socket, keyed like [`Self::tasks`].
    sockets: HashMap<InterceptorId, DnsSocket>,
    /// Finds the socket behind an agent connection, for sockets that fell back to
    /// [`DnsSocketState::Tunneled`].
    tunnels: HashMap<(ConnectionId, NetProtocol), InterceptorId>,
    /// Queries waiting on a remote lookup, and the socket to answer on.
    queries: HashMap<DnsQueryId, (InterceptorId, PendingQuery)>,
    /// Source of [`InterceptorId::connection_id`]s and [`DnsQueryId`]s.
    next_id: u64,
}

/// A DNS socket whose traffic the proxy is answering itself instead of tunneling.
struct DnsSocket {
    layer_id: LayerId,
    /// Where the application wanted to send its queries. Only needed if we end up tunneling.
    remote_address: SocketAddress,
    /// Owns the local socket. Dropping it closes the connection with the application.
    task: TaskSender<Interceptor>,
    framing: DnsFraming,
    state: DnsSocketState,
}

enum DnsSocketState {
    /// Queries are being parsed and answered here.
    Filtering,
    /// A query arrived that we cannot answer, so we asked the agent to connect us to the DNS
    /// server the application chose. Everything read meanwhile is held here, in order.
    Connecting { buffered: VecDeque<Bytes> },
    /// Raw bytes are passing through to the agent.
    Tunneled { connection_id: ConnectionId },
}

/// Asks the [`OutgoingProxy`] to connect a DNS socket to the server the application chose.
///
/// The connection has to be made by the proxy rather than here, because matching the agent's
/// response to it goes through the proxy's request queues.
pub(super) struct DnsTunnelRequest {
    pub id: InterceptorId,
    pub layer_id: LayerId,
    pub remote_address: SocketAddress,
}

impl DnsFiltering {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            tasks: Default::default(),
            sockets: Default::default(),
            tunnels: Default::default(),
            queries: Default::default(),
            next_id: Default::default(),
        }
    }

    /// Points the socket tasks at the current agent connection. Must be called before any
    /// other method, and again after a reconnect.
    pub(super) fn attach(&mut self, message_bus: &MessageBus<OutgoingProxy>) {
        match &mut self.tasks {
            Some(tasks) => tasks.set_agent_tx(message_bus.clone_agent_tx()),
            None => self.tasks = Some(BackgroundTasks::new(message_bus.clone_agent_tx())),
        }
    }

    /// Whether this connect request is DNS traffic we should answer ourselves.
    pub(super) fn should_intercept(&self, request: &OutgoingConnectRequest) -> bool {
        self.enabled
            && matches!(request.remote_address, SocketAddress::Ip(addr) if addr.port() == DNS_PORT)
            && matches!(
                request.protocol,
                NetProtocol::Datagrams | NetProtocol::Stream
            )
    }

    /// Takes over a connection to a DNS server.
    ///
    /// Opens the local socket and answers the layer right away. Nothing is sent to the agent
    /// unless a query turns up that we cannot resolve.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), err)]
    pub(super) async fn intercept_connection(
        &mut self,
        connection_id: u128,
        message_id: MessageId,
        layer_id: LayerId,
        request: OutgoingConnectRequest,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> io::Result<()> {
        let protocol = request.protocol;
        let prepared_socket = protocol
            .prepare_socket(request.remote_address.clone())
            .await?;
        let layer_address = prepared_socket.local_address()?;

        let id = InterceptorId {
            connection_id: self.take_id(),
            protocol,
        };

        let task = self
            .tasks
            .as_mut()
            .expect("attach was not called")
            .register(
                Interceptor::new(
                    id,
                    prepared_socket,
                    Arc::new(Semaphore::new(WRITE_BUDGET_BYTES)),
                ),
                id,
                OutgoingProxy::CHANNEL_SIZE,
            );

        self.sockets.insert(
            id,
            DnsSocket {
                layer_id,
                remote_address: request.remote_address,
                task,
                framing: DnsFraming::new(protocol),
                state: DnsSocketState::Filtering,
            },
        );

        message_bus
            .send(ToLayer {
                message_id,
                layer_id,
                message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Ok(
                    OutgoingConnectResponse {
                        connection_id,
                        layer_address,
                        in_cluster_address: None,
                    },
                ))),
            })
            .await;

        Ok(())
    }

    /// Next update from one of the socket tasks. For use in the proxy's `select!`.
    pub(super) async fn next_update(
        &mut self,
    ) -> Option<(
        InterceptorId,
        TaskUpdate<(Bytes, OwnedSemaphorePermit), io::Error>,
    )> {
        self.tasks.as_mut()?.next().await
    }

    /// Handles bytes read from an intercepted socket.
    ///
    /// Returns a request to open the real connection when a query arrives that we cannot
    /// answer, in which case this socket stops being served here.
    #[tracing::instrument(level = Level::TRACE, skip(self, bytes, message_bus), fields(bytes = bytes.len()))]
    pub(super) async fn handle_read(
        &mut self,
        id: InterceptorId,
        bytes: Bytes,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> Option<DnsTunnelRequest> {
        let socket = self.sockets.get_mut(&id)?;

        match &mut socket.state {
            DnsSocketState::Tunneled { connection_id } => {
                let message = id.protocol.wrap_agent_write(*connection_id, bytes);
                message_bus.send_agent(message).await;
                return None;
            }
            DnsSocketState::Connecting { buffered } => {
                buffered.push_back(bytes);
                return None;
            }
            // A zero-sized read means the application shut down writing. There is nothing to
            // forward, and any answer still in flight can still be written back.
            DnsSocketState::Filtering if bytes.is_empty() => return None,
            DnsSocketState::Filtering => {}
        }

        socket.framing.push(&bytes);

        let mut queries = Vec::new();
        let mut fallback = None;
        while let Some(message) = socket.framing.next_message() {
            match intercept(&message, id.protocol) {
                Intercepted::Resolvable(query) => queries.push(*query),
                Intercepted::Unsupported(reason) => {
                    fallback = Some((reason, socket.framing.frame(message.into())));
                    break;
                }
            }
        }

        let tunnel_request = fallback.map(|(reason, message)| {
            tracing::debug!(
                %id,
                reason,
                "Cannot answer this DNS query, falling back to tunneling the connection",
            );

            // Whatever is left in the framing buffer belongs to the application's stream just
            // as much as the message that triggered the fallback, and has to keep its place.
            let buffered = [message, socket.framing.buffered()]
                .into_iter()
                .filter(|bytes| bytes.is_empty().not())
                .collect();
            socket.state = DnsSocketState::Connecting { buffered };

            DnsTunnelRequest {
                id,
                layer_id: socket.layer_id,
                remote_address: socket.remote_address.clone(),
            }
        });

        for query in queries {
            let query_id = DnsQueryId(self.take_id());
            let request = query.lookup_request();
            tracing::debug!(%id, host = request.node, "Resolving intercepted DNS query");

            self.queries.insert(query_id, (id, query));
            message_bus
                .send(DnsFilteringLookup {
                    id: query_id,
                    request,
                })
                .await;
        }

        tunnel_request
    }

    /// Writes the answer to a resolved query back to the application.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(super) async fn handle_lookup_result(&mut self, result: DnsFilteringLookupResult) {
        let Some((id, query)) = self.queries.remove(&result.id) else {
            tracing::trace!("Lookup finished for a DNS socket that is already gone");
            return;
        };

        let response = match result.result {
            Ok(lookup) => query.answer(lookup),
            Err(error) => {
                tracing::warn!(%error, host = query.host(), "Failed to resolve an intercepted DNS query");
                query.error(ResponseCode::ServFail)
            }
        };

        let Some(socket) = self.sockets.get(&id) else {
            return;
        };

        socket
            .task
            .send(InterceptorCommand::Data(socket.framing.frame(response)))
            .await;
    }

    /// Wires up a socket that fell back to tunneling, flushing what it buffered meanwhile.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus))]
    pub(super) async fn handle_tunnel_connected(
        &mut self,
        id: InterceptorId,
        connect: RemoteResult<DaemonConnect>,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) {
        let Some(socket) = self.sockets.get_mut(&id) else {
            return;
        };

        let connection_id = match connect {
            Ok(connect) => connect.connection_id,
            Err(error) => {
                tracing::warn!(
                    %id, %error,
                    "Failed to connect an intercepted DNS socket to the server the application \
                    chose, closing it",
                );
                self.close(&id, message_bus).await;
                return;
            }
        };

        let buffered = match std::mem::replace(
            &mut socket.state,
            DnsSocketState::Tunneled { connection_id },
        ) {
            DnsSocketState::Connecting { buffered } => buffered,
            _ => Default::default(),
        };

        self.tunnels.insert((connection_id, id.protocol), id);

        for bytes in buffered {
            let message = id.protocol.wrap_agent_write(connection_id, bytes);
            message_bus.send_agent(message).await;
        }
    }

    /// Whether this agent connection was opened for a socket that fell back to tunneling.
    pub(super) fn owns_connection(
        &self,
        connection_id: ConnectionId,
        protocol: NetProtocol,
    ) -> bool {
        self.tunnels.contains_key(&(connection_id, protocol))
    }

    /// Passes data from the agent to a tunneled socket.
    pub(super) async fn handle_agent_read(
        &mut self,
        connection_id: ConnectionId,
        protocol: NetProtocol,
        bytes: Bytes,
    ) {
        let Some(socket) = self
            .tunnels
            .get(&(connection_id, protocol))
            .and_then(|id| self.sockets.get(id))
        else {
            return;
        };

        socket.task.send(InterceptorCommand::Data(bytes)).await;
    }

    /// Returns whether the closed connection belongs to this feature.
    pub(super) async fn handle_agent_close(
        &mut self,
        connection_id: ConnectionId,
        protocol: NetProtocol,
    ) -> bool {
        let Some(id) = self.tunnels.remove(&(connection_id, protocol)) else {
            return false;
        };

        if let Some(socket) = self.sockets.get(&id) {
            socket.task.send(InterceptorCommand::Shutdown).await;
        }

        true
    }

    /// Handles a socket task that exited, i.e. the application closed the connection.
    pub(super) async fn handle_finished(
        &mut self,
        id: InterceptorId,
        result: Result<(), TaskError<io::Error>>,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) {
        match result {
            Ok(()) => tracing::debug!(%id, "Intercepted DNS socket finished"),
            Err(TaskError::Error(error)) => {
                tracing::warn!(%id, %error, "Intercepted DNS socket failed")
            }
            Err(TaskError::Panic) => tracing::error!(%id, "Intercepted DNS socket panicked"),
        }

        self.close(&id, message_bus).await;
    }

    /// Forgets a socket, telling the agent about it if the connection had reached it.
    async fn close(&mut self, id: &InterceptorId, message_bus: &mut MessageBus<OutgoingProxy>) {
        let Some(socket) = self.sockets.remove(id) else {
            return;
        };

        self.queries.retain(|_, (socket_id, _)| socket_id != id);

        if let DnsSocketState::Tunneled { connection_id } = socket.state {
            self.tunnels.remove(&(connection_id, id.protocol));
            message_bus
                .send_agent(id.protocol.wrap_agent_close(connection_id))
                .await;
        }
    }

    pub(super) fn layer_closed(&mut self, layer_id: LayerId) {
        let sockets = &mut self.sockets;
        sockets.retain(|_, socket| socket.layer_id != layer_id);
        self.tunnels.retain(|_, id| sockets.contains_key(id));
        self.queries.retain(|_, (id, _)| sockets.contains_key(id));
    }

    /// Drops every intercepted socket, for when the agent connection is being replaced.
    pub(super) fn clear(&mut self) {
        if let Some(tasks) = self.tasks.as_mut() {
            tasks.clear();
        }
        self.sockets.clear();
        self.tunnels.clear();
        self.queries.clear();
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Reassembles the byte stream coming from the application into whole DNS messages.
///
/// Over UDP every datagram is exactly one message. Over TCP the messages are prefixed with
/// their length as a big-endian `u16` and a single read may contain a partial message, one
/// message, or several ([RFC 1035 4.2.2](https://www.rfc-editor.org/rfc/rfc1035#section-4.2.2)).
#[derive(Debug)]
pub struct DnsFraming {
    protocol: NetProtocol,
    buffer: BytesMut,
}

impl DnsFraming {
    pub fn new(protocol: NetProtocol) -> Self {
        Self {
            protocol,
            buffer: BytesMut::new(),
        }
    }

    /// Takes the bytes that are not yet a whole message.
    ///
    /// Used when a connection falls back to tunneling, so that a partially received message is
    /// not lost.
    pub fn buffered(&mut self) -> Bytes {
        self.buffer.split().freeze()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Takes the next whole message, if one has arrived.
    pub fn next_message(&mut self) -> Option<Bytes> {
        match self.protocol {
            NetProtocol::Datagrams => self
                .buffer
                .is_empty()
                .not()
                .then(|| self.buffer.split().freeze()),
            NetProtocol::Stream | NetProtocol::Seqpacket => {
                let length = u16::from_be_bytes(self.buffer.first_chunk::<2>().copied()?).into();
                if self.buffer.len() < 2 + length {
                    return None;
                }

                self.buffer.advance(2);
                Some(self.buffer.split_to(length).freeze())
            }
        }
    }

    /// Prepares a serialized message to be written back to the application, adding the length
    /// prefix when the transport calls for one.
    pub fn frame(&self, message: Vec<u8>) -> Bytes {
        match self.protocol {
            NetProtocol::Datagrams => message.into(),
            NetProtocol::Stream | NetProtocol::Seqpacket => {
                let length = u16::try_from(message.len()).unwrap_or(u16::MAX);
                let mut framed = BytesMut::with_capacity(2 + message.len());
                framed.extend_from_slice(&length.to_be_bytes());
                framed.extend_from_slice(&message);
                framed.freeze()
            }
        }
    }
}

/// What we decided to do with one message received from the application.
#[derive(Debug)]
pub enum Intercepted {
    /// A query we can answer from mirrord's remote DNS resolution.
    Resolvable(Box<PendingQuery>),
    /// Something we have no business answering, so the connection falls back to tunneling the
    /// raw bytes to the target. Carries the reason, for logging.
    Unsupported(&'static str),
}

/// Decides whether `message` is a query we can serve ourselves.
pub fn intercept(message: &[u8], protocol: NetProtocol) -> Intercepted {
    let Ok(request) = Message::from_vec(message) else {
        return Intercepted::Unsupported("message is not valid DNS");
    };

    if request.metadata.message_type != MessageType::Query
        || request.metadata.op_code != OpCode::Query
    {
        return Intercepted::Unsupported("not a standard query");
    }

    // Multi-question messages are not used in practice, and their failure semantics are
    // ambiguous, since there is one response code for the whole message. Not worth guessing at.
    let [query] = request.queries.as_slice() else {
        return Intercepted::Unsupported("expected exactly one question");
    };

    if query.query_class() != DNSClass::IN {
        return Intercepted::Unsupported("only the IN class can be resolved remotely");
    }

    let family = match query.query_type() {
        RecordType::A => AddressFamily::Ipv4Only,
        RecordType::AAAA => AddressFamily::Ipv6Only,
        // Remote resolution only ever gives us addresses, so any other record type has to be
        // answered by a real DNS server.
        _ => return Intercepted::Unsupported("only A and AAAA can be resolved remotely"),
    };

    Intercepted::Resolvable(Box::new(PendingQuery {
        request,
        family,
        protocol,
    }))
}

/// A query taken off the application's socket, waiting on a remote lookup.
///
/// Holds on to the original message because the response has to echo its id and its question
/// section for the resolver to accept it.
#[derive(Debug)]
pub struct PendingQuery {
    request: Message,
    family: AddressFamily,
    protocol: NetProtocol,
}

impl PendingQuery {
    /// The name being queried, in the form the agent expects: no trailing root label.
    pub fn host(&self) -> String {
        self.query()
            .name()
            .to_ascii()
            .trim_end_matches('.')
            .to_owned()
    }

    fn query(&self) -> &hickory_proto::op::Query {
        self.request
            .queries
            .first()
            .expect("query section was checked to hold exactly one question")
    }

    /// The remote lookup that answers this query.
    pub fn lookup_request(&self) -> GetAddrInfoRequestV2 {
        GetAddrInfoRequestV2 {
            node: self.host(),
            service_port: 0,
            family: self.family,
            socktype: SockType::Any,
            flags: 0,
            protocol: 0,
        }
    }

    /// Builds the response carrying `lookup`'s addresses.
    ///
    /// Addresses of the wrong family are dropped rather than converted: a resolver that asked
    /// for `A` records cannot do anything with an IPv6 address, and answering with one would
    /// look like a malformed response.
    pub fn answer(&self, lookup: DnsLookup) -> Vec<u8> {
        let name = self.query().name().clone();
        let answers = lookup
            .into_iter()
            .filter_map(|record| match (record.ip, self.family) {
                (IpAddr::V4(ip), AddressFamily::Ipv4Only) => Some(RData::A(A(ip))),
                (IpAddr::V6(ip), AddressFamily::Ipv6Only) => Some(RData::AAAA(AAAA(ip))),
                _ => None,
            })
            .map(|rdata| Record::from_rdata(name.clone(), ANSWER_TTL, rdata))
            .collect::<Vec<_>>();

        // An empty answer section with NOERROR means "the name exists, but not with this record
        // type", which is exactly right when e.g. an IPv4-only service is asked for its AAAA.
        // NXDOMAIN would tell the resolver the name does not exist at all, and resolvers cache
        // that for the whole name.
        let mut response = self.response(ResponseCode::NoError);
        response.add_answers(answers);

        self.serialize(response)
    }

    /// Builds a response that carries `code` and no records.
    pub fn error(&self, code: ResponseCode) -> Vec<u8> {
        self.serialize(self.response(code))
    }

    fn response(&self, code: ResponseCode) -> Message {
        let mut response = Message::response(self.request.metadata.id, OpCode::Query);
        response.metadata.response_code = code;
        response.metadata.recursion_desired = self.request.metadata.recursion_desired;
        response.metadata.recursion_available = true;
        response.add_query(self.query().clone());
        response
    }

    /// Serializes `response`, truncating it if it would not fit the transport.
    ///
    /// A resolver that receives a truncated response retries the query over TCP, where the size
    /// limit does not apply.
    fn serialize(&self, response: Message) -> Vec<u8> {
        let Ok(bytes) = response.to_vec() else {
            return self.servfail();
        };

        if self.protocol != NetProtocol::Datagrams || bytes.len() <= self.max_udp_payload() {
            return bytes;
        }

        response
            .truncate()
            .to_vec()
            .unwrap_or_else(|_| self.servfail())
    }

    /// Last resort for the serialization failures below, which should not be reachable:
    /// every message we serialize is one we just built ourselves.
    ///
    /// A SERVFAIL still beats writing nothing back, which would leave the application waiting
    /// out its own timeout.
    fn servfail(&self) -> Vec<u8> {
        Message::error_msg(
            self.request.metadata.id,
            OpCode::Query,
            ResponseCode::ServFail,
        )
        .to_vec()
        .unwrap_or_default()
    }

    /// How large a UDP response the resolver said it can accept.
    fn max_udp_payload(&self) -> usize {
        self.request
            .edns
            .as_ref()
            .map(|edns| usize::from(edns.max_payload()))
            .unwrap_or(MAX_UDP_PAYLOAD)
            .max(MAX_UDP_PAYLOAD)
    }
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use hickory_proto::{
        op::{Message, Query},
        rr::Name,
    };
    use mirrord_protocol::dns::LookupRecord;

    use super::*;

    fn query_bytes(name: &str, record_type: RecordType) -> Vec<u8> {
        let mut message = Message::query();
        message.metadata.id = 1337;
        message.metadata.recursion_desired = true;
        message.add_query(Query::query(Name::from_ascii(name).unwrap(), record_type));
        message.to_vec().unwrap()
    }

    fn unwrap_resolvable(intercepted: Intercepted) -> PendingQuery {
        match intercepted {
            Intercepted::Resolvable(query) => *query,
            Intercepted::Unsupported(reason) => panic!("expected a resolvable query: {reason}"),
        }
    }

    #[test]
    fn resolves_a_query_into_a_lookup() {
        let bytes = query_bytes("kubernetes.default.svc.cluster.local.", RecordType::A);
        let query = unwrap_resolvable(intercept(&bytes, NetProtocol::Datagrams));

        assert_eq!(query.host(), "kubernetes.default.svc.cluster.local");
        let request = query.lookup_request();
        assert_eq!(request.node, "kubernetes.default.svc.cluster.local");
        assert_eq!(request.family, AddressFamily::Ipv4Only);
    }

    #[test]
    fn answer_echoes_the_question_and_carries_the_address() {
        let bytes = query_bytes("my-service.default.svc.cluster.local.", RecordType::A);
        let query = unwrap_resolvable(intercept(&bytes, NetProtocol::Datagrams));

        let answer = query.answer(DnsLookup(vec![LookupRecord {
            name: "my-service.default.svc.cluster.local".to_owned(),
            ip: Ipv4Addr::new(10, 24, 0, 1).into(),
        }]));

        let response = Message::from_vec(&answer).unwrap();
        assert_eq!(response.metadata.id, 1337);
        assert_eq!(response.metadata.message_type, MessageType::Response);
        assert_eq!(response.metadata.response_code, ResponseCode::NoError);
        assert!(response.metadata.recursion_desired);
        assert_eq!(response.queries.len(), 1);
        assert_eq!(
            &response.answers.first().unwrap().data,
            &RData::A(A(Ipv4Addr::new(10, 24, 0, 1)))
        );
    }

    /// The agent resolves both families at once for some hosts, but a resolver that asked for
    /// `A` records cannot use an IPv6 answer.
    #[test]
    fn answer_drops_addresses_of_the_other_family() {
        let bytes = query_bytes("dual-stack.default.svc.cluster.local.", RecordType::AAAA);
        let query = unwrap_resolvable(intercept(&bytes, NetProtocol::Datagrams));

        let answer = query.answer(DnsLookup(vec![
            LookupRecord {
                name: "dual-stack.default.svc.cluster.local".to_owned(),
                ip: Ipv4Addr::new(10, 24, 0, 1).into(),
            },
            LookupRecord {
                name: "dual-stack.default.svc.cluster.local".to_owned(),
                ip: Ipv6Addr::LOCALHOST.into(),
            },
        ]));

        let response = Message::from_vec(&answer).unwrap();
        assert_eq!(response.answers.len(), 1);
        assert_eq!(
            &response.answers.first().unwrap().data,
            &RData::AAAA(AAAA(Ipv6Addr::LOCALHOST))
        );
    }

    /// A name that resolves to nothing of the requested type is NOERROR with no answers, not
    /// NXDOMAIN. See the comment in [`PendingQuery::answer`].
    #[test]
    fn empty_lookup_is_a_noerror_with_no_answers() {
        let bytes = query_bytes("nothing.default.svc.cluster.local.", RecordType::A);
        let query = unwrap_resolvable(intercept(&bytes, NetProtocol::Datagrams));

        let response = Message::from_vec(&query.answer(DnsLookup(vec![]))).unwrap();
        assert_eq!(response.metadata.response_code, ResponseCode::NoError);
        assert!(response.answers.is_empty());
    }

    #[test]
    fn record_types_we_cannot_resolve_are_not_intercepted() {
        for record_type in [RecordType::SRV, RecordType::TXT, RecordType::MX] {
            let bytes = query_bytes("_grpc._tcp.default.svc.cluster.local.", record_type);
            assert!(
                matches!(
                    intercept(&bytes, NetProtocol::Datagrams),
                    Intercepted::Unsupported(..)
                ),
                "{record_type} should not be intercepted"
            );
        }
    }

    #[test]
    fn garbage_is_not_intercepted() {
        assert!(matches!(
            intercept(b"definitely not dns", NetProtocol::Datagrams),
            Intercepted::Unsupported(..)
        ));
    }

    /// Over UDP a response longer than the resolver's advertised limit must come back
    /// truncated, so the resolver retries over TCP instead of reading a cut-off message.
    #[test]
    fn oversized_udp_answer_is_truncated() {
        let bytes = query_bytes("many.default.svc.cluster.local.", RecordType::A);
        let query = unwrap_resolvable(intercept(&bytes, NetProtocol::Datagrams));

        let answer = query.answer(many_addresses());
        assert!(answer.len() <= MAX_UDP_PAYLOAD);

        let response = Message::from_vec(&answer).unwrap();
        assert!(response.metadata.truncation);
    }

    /// The same response over TCP has no size limit, so nothing is dropped.
    #[test]
    fn oversized_tcp_answer_is_not_truncated() {
        let bytes = query_bytes("many.default.svc.cluster.local.", RecordType::A);
        let query = unwrap_resolvable(intercept(&bytes, NetProtocol::Stream));

        let response = Message::from_vec(&query.answer(many_addresses())).unwrap();
        assert!(!response.metadata.truncation);
        assert_eq!(response.answers.len(), usize::from(u8::MAX));
    }

    fn many_addresses() -> DnsLookup {
        DnsLookup(
            (0..u8::MAX)
                .map(|i| LookupRecord {
                    name: "many.default.svc.cluster.local".to_owned(),
                    ip: Ipv4Addr::new(10, 24, 0, i).into(),
                })
                .collect(),
        )
    }

    #[test]
    fn udp_framing_yields_one_message_per_datagram() {
        let mut framing = DnsFraming::new(NetProtocol::Datagrams);
        assert!(framing.next_message().is_none());

        let bytes = query_bytes("a.cluster.local.", RecordType::A);
        framing.push(&bytes);
        assert_eq!(framing.next_message().unwrap(), Bytes::from(bytes));
        assert!(framing.next_message().is_none());
    }

    /// TCP gives us a stream, so a message can arrive in pieces, and several can arrive at once.
    #[test]
    fn tcp_framing_reassembles_length_prefixed_messages() {
        let mut framing = DnsFraming::new(NetProtocol::Stream);
        let first = query_bytes("a.cluster.local.", RecordType::A);
        let second = query_bytes("b.cluster.local.", RecordType::A);

        let mut stream = framing.frame(first.clone()).to_vec();
        stream.extend_from_slice(&framing.frame(second.clone()));

        // Split mid-way through the first message.
        let (head, tail) = stream.split_at(5);
        framing.push(head);
        assert!(framing.next_message().is_none());

        framing.push(tail);
        assert_eq!(framing.next_message().unwrap(), Bytes::from(first));
        assert_eq!(framing.next_message().unwrap(), Bytes::from(second));
        assert!(framing.next_message().is_none());
    }

    /// What is still buffered has to be handed to the tunnel when a connection falls back,
    /// otherwise the query that triggered the fallback is lost.
    #[test]
    fn framing_hands_over_buffered_bytes() {
        let mut framing = DnsFraming::new(NetProtocol::Stream);
        framing.push(&[0, 200, 1, 2, 3]);

        assert!(framing.next_message().is_none());
        assert_eq!(framing.buffered(), Bytes::from_static(&[0, 200, 1, 2, 3]));
        assert!(framing.buffered().is_empty());
    }
}
