use std::{fmt, time::Duration};

use mirrord_protocol::{ClientMessage, DaemonMessage, CLIENT_READY_FOR_LOGS};
use tokio::net::TcpStream;

use crate::{
    agent_conn::AgentConnection,
    background_tasks::{BackgroundTasks, TaskError, TaskSender, TaskUpdate},
    error::{IntProxyError, Result},
    layer_conn::LayerConnection,
    ping_pong::{AgentMessageNotification, PingPong},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage, NOT_A_RESPONSE},
    proxies::{
        incoming::{IncomingProxy, IncomingProxyMessage},
        outgoing::{OutgoingProxy, OutgoingProxyMessage},
        simple::{SimpleProxy, SimpleProxyMessage},
    },
    IntProxy,
};

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
    /// For [`LayerConnection`].
    LayerConnection,
}

impl fmt::Display for MainTaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::SimpleProxy => "SIMPLE_PROXY",
            Self::OutgoingProxy => "OUTGOING_PROXY",
            Self::PingPong => "PING_PONG",
            Self::AgentConnection => "AGENT_CONNECTION",
            Self::LayerConnection => "LAYER_CONNECTION",
            Self::IncomingProxy => "INCOMING_PROXY",
        };

        f.write_str(as_str)
    }
}

/// Messages sent back to the [`ProxySession`] from the main background tasks. See [`MainTaskId`].
#[derive(Debug)]
pub enum ProxyMessage {
    /// Message to be sent to the agent. Handled by [`AgentConnection`].
    ToAgent(ClientMessage),
    /// Message to be sent to the layer. Handled by [`LayerConnection`].
    ToLayer(LocalMessage<ProxyToLayerMessage>),
    /// Message received from the agent. Handled by one of the main background tasks.
    FromAgent(DaemonMessage),
    /// Message received from the layer. Handled by one of the main background tasks.
    FromLayer(LocalMessage<LayerToProxyMessage>),
}

/// [`TaskSender`]s for main background tasks. See [`MainTaskId`].
struct Handlers {
    /// For [`AgentConnection`].
    agent: TaskSender<ClientMessage>,
    /// For [`LayerConnection`].
    layer: TaskSender<LocalMessage<ProxyToLayerMessage>>,
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
pub struct ProxySession {
    handlers: Handlers,
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
}

impl ProxySession {
    /// How long can the `proxy <-> agent` connection remain silent.
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    /// For creating communicating with the main background tasks. See [`MainTaskId`].
    const CHANNEL_SIZE: usize = 512;

    /// Crates a new session. This session will use the provided `stream` to talk with the layer
    /// instance.
    pub async fn new(intproxy: &IntProxy, stream: TcpStream) -> Result<Self> {
        let agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;

        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
            Default::default();
        let agent =
            background_tasks.register(agent_conn, MainTaskId::AgentConnection, Self::CHANNEL_SIZE);
        let layer = background_tasks.register(
            LayerConnection::new(stream),
            MainTaskId::LayerConnection,
            Self::CHANNEL_SIZE,
        );
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
            IncomingProxy::new(&intproxy.config)?,
            MainTaskId::IncomingProxy,
            Self::CHANNEL_SIZE,
        );

        Ok(Self {
            background_tasks,
            handlers: Handlers {
                agent,
                layer,
                simple,
                outgoing,
                incoming,
                ping_pong,
            },
        })
    }

    /// Handles proxying between the connected layer and the agent.
    /// Runs until the layer closes the connection or an error occurs.
    pub async fn serve_connection(mut self) -> Result<()> {
        self.handlers
            .agent
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        let res = loop {
            match self.background_tasks.next().await.unwrap() {
                (MainTaskId::LayerConnection, TaskUpdate::Finished(Ok(()))) => {
                    tracing::info!("layer closed connection, exiting");
                    break Ok(());
                }
                (task_id, TaskUpdate::Finished(Ok(()))) => {
                    tracing::error!("task {task_id} exited unexpectedly");
                    break Err(IntProxyError::TaskExit(task_id));
                }
                (task_id, TaskUpdate::Finished(Err(TaskError::Panic))) => {
                    tracing::error!("task {task_id} panicked");
                    break Err(IntProxyError::TaskPanic(task_id));
                }
                (task_id, TaskUpdate::Finished(Err(TaskError::Error(e)))) => {
                    tracing::error!("task {task_id} failed: {e}");
                    break Err(e);
                }
                (_, TaskUpdate::Message(msg)) => {
                    if let Err(e) = self.handle(msg).await {
                        break Err(e);
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

    /// Routes a [`ProxyMessage`] to the correct background task.
    async fn handle(&mut self, msg: ProxyMessage) -> Result<()> {
        match msg {
            ProxyMessage::FromAgent(msg) => self.handle_agent_message(msg).await?,
            ProxyMessage::FromLayer(msg) => self.handle_layer_message(msg).await?,
            ProxyMessage::ToAgent(msg) => self.handlers.agent.send(msg).await,
            ProxyMessage::ToLayer(msg) => self.handlers.layer.send(msg).await,
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
            DaemonMessage::Close(reason) => return Err(IntProxyError::AgentFailed(reason)),
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
                self.handlers
                    .layer
                    .send(LocalMessage {
                        message_id: NOT_A_RESPONSE,
                        inner: ProxyToLayerMessage::AgentLog(log),
                    })
                    .await;
            }
            other => {
                return Err(IntProxyError::UnexpectedAgentMessage(other));
            }
        }

        Ok(())
    }

    /// Routes a message from the layer to the correct background task.
    /// [`LayerToProxyMessage::NewSession`] is an exception, as it is handled here.
    async fn handle_layer_message(&self, message: LocalMessage<LayerToProxyMessage>) -> Result<()> {
        match message.inner {
            LayerToProxyMessage::NewSession(..) => todo!(),
            LayerToProxyMessage::File(req) => {
                self.handlers
                    .simple
                    .send(SimpleProxyMessage::FileReq(message.message_id, req))
                    .await;
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.handlers
                    .simple
                    .send(SimpleProxyMessage::AddrInfoReq(message.message_id, req))
                    .await
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.handlers
                    .outgoing
                    .send(OutgoingProxyMessage::LayerConnect(req, message.message_id))
                    .await
            }
            LayerToProxyMessage::Incoming(req) => {
                self.handlers
                    .incoming
                    .send(IncomingProxyMessage::LayerRequest(message.message_id, req))
                    .await
            }
        }

        Ok(())
    }
}
