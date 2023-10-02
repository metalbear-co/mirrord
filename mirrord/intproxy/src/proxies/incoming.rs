use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use mirrord_config::feature::network::incoming::IncomingConfig;
use mirrord_protocol::{
    tcp::{
        DaemonTcp, Filter, HttpFilter, HttpRequest, InternalHttpBody, LayerTcp, LayerTcpSteal,
        StealType,
    },
    ClientMessage, ConnectionId, Port, RemoteResult,
};
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{
        IncomingRequest, LocalMessage, MessageId, PortSubscribe, PortUnsubscribe,
        ProxyToLayerMessage,
    },
    request_queue::RequestQueue,
};

mod original;

pub enum LayerHttpFilter {
    /// No filter
    None,
    /// HTTP Header match, to be removed once deprecated.
    HeaderDeprecated(Filter),
    /// New filter.
    Filter(HttpFilter),
}

impl LayerHttpFilter {
    fn new(config: &IncomingConfig) -> Self {
        match (
            &config.http_filter.path_filter,
            &config.http_filter.header_filter,
            &config.http_header_filter.filter,
        ) {
            (Some(path), None, None) => {
                Self::Filter(HttpFilter::Path(Filter::new(path.into()).unwrap()))
            }
            (None, Some(header), None) => {
                Self::Filter(HttpFilter::Header(Filter::new(header.into()).unwrap()))
            }
            (None, None, Some(header)) => {
                Self::HeaderDeprecated(Filter::new(header.into()).unwrap())
            }
            (None, None, None) => Self::None,
            _ => {
                unreachable!("Only one HTTP filter can be specified at a time, please report bug, config should fail before this.")
            }
        }
    }
}

pub struct HttpFilterSettings {
    /// The HTTP filter to use.
    filter: LayerHttpFilter,
    /// Ports to filter HTTP on
    ports: HashSet<Port>,
}

impl HttpFilterSettings {
    fn new(config: &IncomingConfig) -> Self {
        let ports = if config.http_header_filter.filter.is_some() {
            config.http_header_filter.ports.as_slice().iter().copied()
        } else {
            config.http_filter.ports.as_slice().iter().copied()
        }
        .collect();

        let filter = LayerHttpFilter::new(config);

        HttpFilterSettings { filter, ports }
    }

    fn steal_type(&self, port: Port) -> StealType {
        if !self.ports.contains(&port) {
            return StealType::All(port);
        }

        match &self.filter {
            LayerHttpFilter::None => StealType::All(port),
            LayerHttpFilter::HeaderDeprecated(filter) => {
                StealType::FilteredHttp(port, filter.clone())
            }
            LayerHttpFilter::Filter(filter) => StealType::FilteredHttpEx(port, filter.clone()),
        }
    }
}

enum Mode {
    Mirror,
    Steal(HttpFilterSettings),
}

impl Mode {
    fn new(config: &IncomingConfig) -> Self {
        if config.is_steal() {
            Self::Steal(HttpFilterSettings::new(config))
        } else {
            Self::Mirror
        }
    }

    fn wrap_subscribe(&self, port: Port) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::PortSubscribe(port)),
            Self::Steal(filter) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(filter.steal_type(port)))
            }
        }
    }
}

enum IncomingData {
    HttpRaw(HttpRequest<Vec<u8>>),
    HttpFramed(HttpRequest<InternalHttpBody>),
    Raw(Vec<u8>),
}

struct ConnectionTask {
    agent_sender: AgentSender,
    connection_id: ConnectionId,
    layer_address: SocketAddr,
    data_rx: Receiver<IncomingData>,
    data_tx: Option<Sender<IncomingData>>,
}

impl ConnectionTask {
    pub fn handle(&self) -> ConnectionHandle {
        let data_tx = self
            .data_tx
            .as_ref()
            .cloned()
            .expect("should not be none now");
        ConnectionHandle { data_tx }
    }

    pub fn new(
        agent_sender: AgentSender,
        connection_id: ConnectionId,
        layer_address: SocketAddr,
    ) -> Self {
        let (data_tx, data_rx) = mpsc::channel(512);

        Self {
            agent_sender,
            connection_id,
            layer_address,
            data_rx,
            data_tx: Some(data_tx),
        }
    }

    pub async fn run(mut self) {
        self.data_tx = None;
        todo!()
    }
}

struct ConnectionHandle {
    data_tx: Sender<IncomingData>,
}

impl ConnectionHandle {
    async fn consume(&self, data: IncomingData) -> Result<()> {
        self.data_tx
            .send(data)
            .await
            .map_err(|_| IntProxyError::IncomingInterceptorFailed)
    }
}

pub struct IncomingProxy {
    mode: Mode,
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    subscriptions_queue: RequestQueue<PortSubscribe>,
    layer_listeners: HashMap<Port, SocketAddr>,
    connections: HashMap<ConnectionId, ConnectionHandle>,
}

impl IncomingProxy {
    pub fn new(
        config: &IncomingConfig,
        agent_sender: AgentSender,
        layer_sender: LayerSender,
    ) -> Self {
        Self {
            mode: Mode::new(config),
            agent_sender,
            layer_sender,
            subscriptions_queue: Default::default(),
            layer_listeners: Default::default(),
            connections: Default::default(),
        }
    }

    async fn pass_to_connection(&mut self, id: ConnectionId, data: IncomingData) -> Result<()> {
        let Some(connection) = self.connections.get(&id) else {
            tracing::trace!("received incoming data for unknown connection id {id}, dropping");
            return Ok(());
        };

        let res = connection.consume(data).await;
        if res.is_err() {
            self.connections.remove(&id);
        }

        Ok(())
    }

    pub async fn handle_layer_request(
        &mut self,
        request: IncomingRequest,
        message_id: MessageId,
    ) -> Result<()> {
        match request {
            IncomingRequest::PortSubscribe(subscribe) => {
                self.handle_subscribe(subscribe, message_id).await
            }
            IncomingRequest::PortUnsubscribe(unsubscribe) => {
                self.handle_unsubscribe(unsubscribe).await
            }
        }
    }

    async fn handle_subscribe(
        &mut self,
        subscribe: PortSubscribe,
        message_id: MessageId,
    ) -> Result<()> {
        if let Some(previous) = self.layer_listeners.get_mut(&subscribe.port) {
            let PortSubscribe { port, listening_on } = subscribe;

            // Since this struct identifies listening sockets by their port (#1558), we can only
            // forward the incoming traffic to one socket with that port.
            tracing::info!(
                "Received listen request for port {port} while already listening. \
                Might be on different address. Sending all incoming traffic to newest socket."
            );

            *previous = listening_on.try_into()?;

            return self
                .layer_sender
                .send(LocalMessage {
                    message_id,
                    inner: ProxyToLayerMessage::IncomingSubscribe(Ok(())),
                })
                .await
                .map_err(Into::into);
        }

        let message = self.mode.wrap_subscribe(subscribe.port);

        self.subscriptions_queue.insert(subscribe, message_id);

        self.agent_sender.send(message).await.map_err(Into::into)
    }

    async fn handle_unsubscribe(&mut self, unsubscribe: PortUnsubscribe) -> Result<()> {
        let PortUnsubscribe { port } = unsubscribe;

        if self.layer_listeners.remove(&port).is_some() {
            self.agent_sender
                .send(ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)))
                .await
                .map_err(Into::into)
        } else {
            Ok(())
        }
    }

    async fn handle_subscribe_result(&mut self, result: RemoteResult<u16>) -> Result<()> {
        let subscription = self.subscriptions_queue.get()?;
        let response = match result {
            Ok(port) => {
                debug_assert_eq!(subscription.request.port, port);
                self.layer_listeners
                    .insert(port, subscription.request.listening_on.try_into()?);

                Ok(())
            }
            Err(err) => Err(err),
        };

        self.layer_sender
            .send(LocalMessage {
                message_id: subscription.id,
                inner: ProxyToLayerMessage::IncomingSubscribe(response),
            })
            .await
            .map_err(Into::into)
    }

    pub async fn handle_agent_message(&mut self, message: DaemonTcp) -> Result<()> {
        match message {
            DaemonTcp::Close(close) => {
                self.connections.remove(&close.connection_id);
                Ok(())
            }
            DaemonTcp::Data(data) => {
                self.pass_to_connection(data.connection_id, IncomingData::Raw(data.bytes))
                    .await
            }
            DaemonTcp::HttpRequest(http_request) => {
                self.pass_to_connection(
                    http_request.connection_id,
                    IncomingData::HttpRaw(http_request),
                )
                .await
            }
            DaemonTcp::HttpRequestFramed(http_request) => {
                self.pass_to_connection(
                    http_request.connection_id,
                    IncomingData::HttpFramed(http_request),
                )
                .await
            }
            DaemonTcp::NewConnection(connection) => {
                // TODO msmolarek
                let Some(listener) = self
                    .layer_listeners
                    .get(&connection.destination_port)
                    .copied()
                else {
                    // new connection after the listener has closed
                    return Ok(());
                };

                let task = ConnectionTask::new(
                    self.agent_sender.clone(),
                    connection.connection_id,
                    listener,
                );

                self.connections
                    .insert(connection.connection_id, task.handle());

                tokio::spawn(task.run());

                Ok(())
            }
            DaemonTcp::SubscribeResult(result) => self.handle_subscribe_result(result).await,
        }
    }
}
