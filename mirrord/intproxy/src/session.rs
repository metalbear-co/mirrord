use std::{collections::HashMap, fmt, time::Duration};

use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, ConnectionId, DaemonMessage, CLIENT_READY_FOR_LOGS};
use thiserror::Error;
use tokio::{net::TcpStream, time};

use crate::{
    agent_conn::{AgentClosedConnection, AgentConnectInfo, AgentConnection, AgentConnectionError},
    background_tasks::{
        BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate,
    },
    codec::CodecError,
    layer_conn::LayerConnection,
    ping_pong::{AgentMessageNotification, PingPong, PingPongError},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage, SessionId, NOT_A_RESPONSE},
    proxies::{
        incoming::{IncomingProxy, IncomingProxyError, IncomingProxyMessage},
        outgoing::{OutgoingProxy, OutgoingProxyError, OutgoingProxyMessage},
        simple::{SimpleProxy, SimpleProxyMessage},
    },
    request_queue::RequestQueueEmpty,
};

#[derive(Error, Debug)]
pub enum ProxySessionError {
    #[error("connecting with agent failed: {0}")]
    AgentConnectFailed(#[from] AgentConnectionError),
    #[error("agent closed connection with error: {0}")]
    AgentFailed(String),
    #[error("agent sent unexpected message: {0:?}")]
    UnexpectedAgentMessage(DaemonMessage),
    #[error("layer sent unexpected message: {0:?}")]
    UnexpectedLayerMessage(LayerToProxyMessage),

    #[error("background task {0} exited unexpectedly")]
    TaskExit(MainTaskId),
    #[error("background task {0} panicked")]
    TaskPanic(MainTaskId),

    #[error("{0}")]
    AgentConnectionError(#[from] AgentClosedConnection),
    #[error("ping pong failed: {0}")]
    PingPong(#[from] PingPongError),
    #[error("layer connection failed: {0}")]
    LayerConnectionError(#[from] CodecError),
    #[error("simple proxy failed: {0}")]
    SimpleProxy(#[from] RequestQueueEmpty),
    #[error("outgoing proxy failed: {0}")]
    OutgoingProxy(#[from] OutgoingProxyError),
    #[error("incoming proxy failed: {0}")]
    IncomingProxy(#[from] IncomingProxyError),
}

pub type Result<T> = core::result::Result<T, ProxySessionError>;

/// Enumerated ids of main [`BackgroundTask`](crate::background_tasks::BackgroundTask)s used by
/// [`ProxySession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainTaskId {
    /// For [`SimpleProxy`].
    SimpleProxy,
    /// For [`OutgoingProxy`].
    OutgoingProxy,
    /// For [`IncomingProxy`].
    IncomingProxy,
    /// For [`PingPong`].
    PingPong,
    /// For [`AgentConnection`].
    AgentConnection,
    /// For [`LayerConnection`]s.
    LayerConnection(SessionId),
}

impl fmt::Display for MainTaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SimpleProxy => f.write_str("SIMPLE_PROXY"),
            Self::OutgoingProxy => f.write_str("OUTGOING_PROXY"),
            Self::PingPong => f.write_str("PING_PONG"),
            Self::AgentConnection => f.write_str("AGENT_CONNECTION"),
            Self::LayerConnection(id) => write!(f, "LAYER_CONNECTION {}", id),
            Self::IncomingProxy => f.write_str("INCOMING_PROXY"),
        }
    }
}

/// Messages sent back to the [`ProxySession`] from the main background tasks. See [`MainTaskId`].
#[derive(Debug)]
pub enum ProxyMessage {
    /// Message to be sent to the agent. Handled by [`AgentConnection`].
    ToAgent(ClientMessage),
    /// Message to be sent to the layer. Handled by [`LayerConnection`].
    ToLayer(LocalMessage<ProxyToLayerMessage>, SessionId),
    /// Message received from the agent. Handled by one of the main background tasks.
    FromAgent(DaemonMessage),
    /// Message received from the layer. Handled by one of the main background tasks.
    FromLayer(LocalMessage<LayerToProxyMessage>, SessionId),
}

/// [`TaskSender`]s for main background tasks. See [`MainTaskId`].
struct Handlers {
    /// For [`LayerConnection`]s.
    layers: HashMap<ConnectionId, TaskSender<LocalMessage<ProxyToLayerMessage>>>,
    /// For [`AgentConnection`].
    agent: TaskSender<ClientMessage>,
    /// For [`SimpleProxy`].
    simple: TaskSender<SimpleProxyMessage>,
    /// For [`OutgoingProxy`].
    outgoing: TaskSender<OutgoingProxyMessage>,
    /// For [`IncomingProxy`].
    incoming: TaskSender<IncomingProxyMessage>,
    /// For [`PingPong`].
    ping_pong: TaskSender<AgentMessageNotification>,
}

/// Single internal proxy session between the layer and the agent.
///
/// Utilizes a [`BackgroundTasks`] manager and several
/// [`BackgroundTask`](crate::background_tasks::BackgroundTask)s to implement proxying logic.
///
/// This session itself is run as a [`BackgroundTask`](crate::background_tasks::BackgroundTask) of
/// the [`IntProxy`].
pub struct ProxySession {
    handlers: Handlers,
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxySessionError>,
}

impl ProxySession {
    /// How long can the `proxy <-> agent` connection remain silent.
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    /// For creating communicating with the main background tasks. See [`MainTaskId`].
    const CHANNEL_SIZE: usize = 512;

    /// Crates a new session.
    pub async fn new(
        config: &LayerConfig,
        agent_connect_info: Option<&AgentConnectInfo>,
    ) -> Result<Self> {
        let agent_conn = AgentConnection::new(config, agent_connect_info).await?;

        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxySessionError> =
            Default::default();
        let agent =
            background_tasks.register(agent_conn, MainTaskId::AgentConnection, Self::CHANNEL_SIZE);
        let ping_pong = background_tasks.register(
            PingPong::new(Self::PING_INTERVAL),
            MainTaskId::PingPong,
            Self::CHANNEL_SIZE,
        );
        let simple = background_tasks.register(
            SimpleProxy::default(),
            MainTaskId::SimpleProxy,
            Self::CHANNEL_SIZE,
        );
        let outgoing = background_tasks.register(
            OutgoingProxy::default(),
            MainTaskId::OutgoingProxy,
            Self::CHANNEL_SIZE,
        );
        let incoming = background_tasks.register(
            IncomingProxy::new(config)?,
            MainTaskId::IncomingProxy,
            Self::CHANNEL_SIZE,
        );

        Ok(Self {
            background_tasks,
            handlers: Handlers {
                layers: Default::default(),
                agent,
                simple,
                outgoing,
                incoming,
                ping_pong,
            },
        })
    }

    /// Routes a [`ProxyMessage`] to the correct background task.
    async fn handle(&mut self, msg: ProxyMessage) -> Result<()> {
        match msg {
            ProxyMessage::FromAgent(msg) => self.handle_agent_message(msg).await?,
            ProxyMessage::FromLayer(msg, session_id) => {
                self.handle_layer_message(msg, session_id).await?
            }
            ProxyMessage::ToAgent(msg) => self.handlers.agent.send(msg).await,
            ProxyMessage::ToLayer(msg, id) => {
                self.handlers.layers.get(&id).expect("TODO").send(msg).await
            }
        }

        Ok(())
    }

    async fn handle_task_update(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, ProxySessionError>,
    ) -> Result<()> {
        match (task_id, update) {
            (MainTaskId::LayerConnection(id), TaskUpdate::Finished(Ok(()))) => {
                tracing::trace!("layer connection {id} closed");
            }
            (task_id, TaskUpdate::Finished(res)) => match res {
                Ok(()) => {
                    tracing::error!("task {task_id} finished unexpectedly");
                    return Err(ProxySessionError::TaskExit(task_id));
                }
                Err(TaskError::Error(e)) => {
                    tracing::error!("task {task_id} failed: {e}");
                    return Err(e);
                }
                Err(TaskError::Panic) => {
                    tracing::error!("task {task_id} panicked");
                    return Err(ProxySessionError::TaskPanic(task_id));
                }
            },
            (_, TaskUpdate::Message(msg)) => self.handle(msg).await?,
        }

        Ok(())
    }

    /// Routes most messages from the agent to the correct background task.
    /// Some messages are handled here.
    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        self.handlers
            .ping_pong
            .send(AgentMessageNotification {
                pong: matches!(message, DaemonMessage::Pong),
            })
            .await;

        match message {
            DaemonMessage::Pong => {}
            DaemonMessage::Close(reason) => return Err(ProxySessionError::AgentFailed(reason)),
            DaemonMessage::TcpOutgoing(msg) => {
                self.handlers
                    .outgoing
                    .send(OutgoingProxyMessage::AgentStream(msg))
                    .await
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.handlers
                    .outgoing
                    .send(OutgoingProxyMessage::AgentDatagrams(msg))
                    .await
            }
            DaemonMessage::File(msg) => {
                self.handlers
                    .simple
                    .send(SimpleProxyMessage::FileRes(msg))
                    .await
            }
            DaemonMessage::GetAddrInfoResponse(msg) => {
                self.handlers
                    .simple
                    .send(SimpleProxyMessage::AddrInfoRes(msg))
                    .await
            }
            DaemonMessage::Tcp(msg) => {
                self.handlers
                    .incoming
                    .send(IncomingProxyMessage::AgentMirror(msg))
                    .await
            }
            DaemonMessage::TcpSteal(msg) => {
                self.handlers
                    .incoming
                    .send(IncomingProxyMessage::AgentSteal(msg))
                    .await
            }
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    self.handlers.agent.send(ClientMessage::ReadyForLogs).await;
                }
            }
            DaemonMessage::LogMessage(log) => {
                if let Some(sender) = self.handlers.layers.values().next() {
                    sender
                        .send(LocalMessage {
                            message_id: NOT_A_RESPONSE,
                            inner: ProxyToLayerMessage::AgentLog(log),
                        })
                        .await;
                }
            }
            other => {
                return Err(ProxySessionError::UnexpectedAgentMessage(other));
            }
        }

        Ok(())
    }

    /// Routes a message from the layer to the correct background task.
    async fn handle_layer_message(
        &self,
        message: LocalMessage<LayerToProxyMessage>,
        session_id: SessionId,
    ) -> Result<()> {
        match message.inner {
            LayerToProxyMessage::File(req) => {
                self.handlers
                    .simple
                    .send(SimpleProxyMessage::FileReq(
                        message.message_id,
                        session_id,
                        req,
                    ))
                    .await;
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.handlers
                    .simple
                    .send(SimpleProxyMessage::AddrInfoReq(
                        message.message_id,
                        session_id,
                        req,
                    ))
                    .await
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.handlers
                    .outgoing
                    .send(OutgoingProxyMessage::LayerConnect(
                        req,
                        message.message_id,
                        session_id,
                    ))
                    .await
            }
            LayerToProxyMessage::Incoming(req) => {
                self.handlers
                    .incoming
                    .send(IncomingProxyMessage::LayerRequest(
                        message.message_id,
                        session_id,
                        req,
                    ))
                    .await
            }
            other => return Err(ProxySessionError::UnexpectedLayerMessage(other)),
        }

        Ok(())
    }
}

pub struct NewSessionStream(pub TcpStream, pub SessionId);

impl BackgroundTask for ProxySession {
    type Error = ProxySessionError;
    type MessageIn = NewSessionStream;
    type MessageOut = ();

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<()> {
        self.handlers
            .agent
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        let res = loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    }
                    Some(NewSessionStream(stream, id)) => {
                        tracing::trace!("new related layer session {id}");
                        let sender = self.background_tasks.register(LayerConnection::new(stream, id), MainTaskId::LayerConnection(id), Self::CHANNEL_SIZE);
                        self.handlers.layers.insert(id, sender);
                    }
                },
                Some((task_id, update)) = self.background_tasks.next() => {
                    if let Err(e) = self.handle_task_update(task_id, update).await {
                        break Err(e);
                    }
                },
                _ = time::sleep(Duration::from_secs(2)), if self.handlers.layers.is_empty() => {
                    if self.handlers.layers.is_empty() {
                        tracing::info!("all layer connections closed, exiting");
                        break Ok(());
                    }
                }
            }
        };

        std::mem::drop(self.handlers);
        let task_results = self.background_tasks.results().await;
        for (id, res) in task_results {
            tracing::trace!("{id} result: {res:?}");
        }

        res
    }
}
