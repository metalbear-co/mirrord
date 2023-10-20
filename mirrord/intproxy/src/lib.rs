#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]
#![warn(clippy::indexing_slicing)]

use std::{collections::HashMap, time::Duration};

use agent_conn::AgentConnection;
use background_tasks::{BackgroundTasks, TaskSender, TaskUpdate};
use layer_conn::LayerConnection;
use layer_initializer::LayerInitializer;
use main_tasks::{FromLayer, LayerForked, MainTaskId, ProxyMessage, ToLayer};
use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage, LogLevel, CLIENT_READY_FOR_LOGS};
use ping_pong::{AgentMessageNotification, PingPong};
use protocol::LayerId;
use proxies::{
    incoming::{IncomingProxy, IncomingProxyMessage},
    outgoing::{OutgoingProxy, OutgoingProxyMessage},
    simple::{SimpleProxy, SimpleProxyMessage},
};
use tokio::{net::TcpListener, time};

use crate::{
    agent_conn::AgentConnectInfo,
    background_tasks::TaskError,
    error::IntProxyError,
    main_tasks::LayerClosed,
    protocol::{LayerToProxyMessage, LocalMessage},
};

pub mod agent_conn;
mod background_tasks;
pub mod codec;
pub mod error;
mod layer_conn;
mod layer_initializer;
mod main_tasks;
mod ping_pong;
pub mod protocol;
mod proxies;
mod remote_resources;
mod request_queue;

/// [`TaskSender`]s for main background tasks. See [`MainTaskId`].
struct TaskTxs {
    layers: HashMap<LayerId, TaskSender<LayerConnection>>,
    _layer_initializer: TaskSender<LayerInitializer>,
    agent: TaskSender<AgentConnection>,
    simple: TaskSender<SimpleProxy>,
    outgoing: TaskSender<OutgoingProxy>,
    incoming: TaskSender<IncomingProxy>,
    ping_pong: TaskSender<PingPong>,
}

/// This struct contains logic for proxying between multiple layer instances and one agent.
/// It maintains a singe agent connection.
///
/// Utilizes multiple [`BackgroundTask`](background_tasks::BackgroundTask)s to split logic of
/// different mirrod features (e.g. file operations and incoming traffic).
pub struct IntProxy {
    any_connection_accepted: bool,
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
    task_txs: TaskTxs,
}

impl IntProxy {
    /// Size of channels used to communicate with main tasks (see [`MainTaskId`]).
    const CHANNEL_SIZE: usize = 512;
    /// How long can the agent connection remain silent.
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    /// Initiates a new agent connection and creates a new [`IntProxy`].
    /// The returned instance will accept connections from the layers using the given
    /// [`TcpListener`].
    pub async fn new(
        config: &LayerConfig,
        agent_connect_info: Option<&AgentConnectInfo>,
        listener: TcpListener,
    ) -> Result<Self, IntProxyError> {
        let agent_conn = AgentConnection::new(config, agent_connect_info).await?;
        Ok(Self::new_with_connection(agent_conn, listener))
    }

    /// Creates a new [`IntProxy`] using existing [`AgentConnection`].
    /// The returned instance will accept connections from the layers using the given
    /// [`TcpListener`].
    pub fn new_with_connection(agent_conn: AgentConnection, listener: TcpListener) -> Self {
        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
            Default::default();

        let agent =
            background_tasks.register(agent_conn, MainTaskId::AgentConnection, Self::CHANNEL_SIZE);
        let layer_initializer = background_tasks.register(
            LayerInitializer::new(listener),
            MainTaskId::LayerInitializer,
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
            IncomingProxy::default(),
            MainTaskId::IncomingProxy,
            Self::CHANNEL_SIZE,
        );

        Self {
            any_connection_accepted: false,
            background_tasks,
            task_txs: TaskTxs {
                layers: Default::default(),
                _layer_initializer: layer_initializer,
                agent,
                simple,
                outgoing,
                incoming,
                ping_pong,
            },
        }
    }

    /// Runs main event loop of this proxy.
    /// Expects to accept the first layer connection within the given `first_timeout`.
    /// Exits after `idle_timeout` when there are no more layer connections.
    pub async fn run(
        mut self,
        first_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<(), IntProxyError> {
        self.task_txs
            .agent
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        loop {
            tokio::select! {
                Some((task_id, task_update)) = self.background_tasks.next() => {
                    self.handle_task_update(task_id, task_update).await?;
                }

                _ = time::sleep(first_timeout), if !self.any_connection_accepted => {
                    if !self.any_connection_accepted {
                        return Err(IntProxyError::ConnectionAcceptTimeout);
                    }
                },

                _ = time::sleep(idle_timeout), if self.any_connection_accepted && self.task_txs.layers.is_empty() => {
                    if self.task_txs.layers.is_empty() {
                        tracing::trace!("intproxy timeout, no active connections. Exiting.");
                        break;
                    }
                },
            }
        }

        std::mem::drop(self.task_txs);
        let results = self.background_tasks.results().await;

        for (task_id, res) in results {
            tracing::trace!("{task_id} result: {res:?}");
        }

        Ok(())
    }

    /// Routes a [`ProxyMessage`] to the correct background task.
    /// [`ProxyMessage::NewLayer`] is handled here, as an exception.
    async fn handle(&mut self, msg: ProxyMessage) -> Result<(), IntProxyError> {
        match msg {
            ProxyMessage::NewLayer(new_layer) => {
                self.any_connection_accepted = true;

                let tx = self.background_tasks.register(
                    LayerConnection::new(new_layer.stream, new_layer.id),
                    MainTaskId::LayerConnection(new_layer.id),
                    Self::CHANNEL_SIZE,
                );
                self.task_txs.layers.insert(new_layer.id, tx);

                if let Some(parent) = new_layer.parent_id {
                    let msg = LayerForked {
                        child: new_layer.id,
                        parent,
                    };

                    self.task_txs
                        .simple
                        .send(SimpleProxyMessage::LayerForked(msg))
                        .await;
                    self.task_txs
                        .incoming
                        .send(IncomingProxyMessage::LayerForked(msg))
                        .await;
                }
            }
            ProxyMessage::FromAgent(msg) => self.handle_agent_message(msg).await?,
            ProxyMessage::FromLayer(msg) => self.handle_layer_message(msg).await?,
            ProxyMessage::ToAgent(msg) => self.task_txs.agent.send(msg).await,
            ProxyMessage::ToLayer(msg) => {
                let ToLayer {
                    message,
                    message_id,
                    layer_id,
                } = msg;

                if let Some(tx) = self.task_txs.layers.get(&layer_id) {
                    tx.send(LocalMessage {
                        message_id,
                        inner: message,
                    })
                    .await;
                }
            }
        }

        Ok(())
    }

    /// Handles a [`TaskUpdate`] from one of the main tasks (see [`MainTaskId`]).
    async fn handle_task_update(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, IntProxyError>,
    ) -> Result<(), IntProxyError> {
        match (task_id, update) {
            (MainTaskId::LayerConnection(LayerId(id)), TaskUpdate::Finished(Ok(()))) => {
                tracing::trace!("layer connection {id} closed");

                let msg = LayerClosed { id: LayerId(id) };

                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::LayerClosed(msg))
                    .await;
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::LayerClosed(msg))
                    .await;

                self.task_txs.layers.remove(&LayerId(id));
            }
            (task_id, TaskUpdate::Finished(res)) => match res {
                Ok(()) => {
                    tracing::error!("task {task_id} finished unexpectedly");
                    return Err(IntProxyError::TaskExit(task_id));
                }
                Err(TaskError::Error(e)) => {
                    tracing::error!("task {task_id} failed: {e}");
                    return Err(e);
                }
                Err(TaskError::Panic) => {
                    tracing::error!("task {task_id} panicked");
                    return Err(IntProxyError::TaskPanic(task_id));
                }
            },
            (_, TaskUpdate::Message(msg)) => self.handle(msg).await?,
        }

        Ok(())
    }

    /// Routes most messages from the agent to the correct background task.
    /// Some messages are handled here.
    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<(), IntProxyError> {
        self.task_txs
            .ping_pong
            .send(AgentMessageNotification {
                pong: matches!(message, DaemonMessage::Pong),
            })
            .await;

        match message {
            DaemonMessage::Pong => {}
            DaemonMessage::Close(reason) => return Err(IntProxyError::AgentFailed(reason)),
            DaemonMessage::TcpOutgoing(msg) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::AgentStream(msg))
                    .await
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::AgentDatagrams(msg))
                    .await
            }
            DaemonMessage::File(msg) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::FileRes(msg))
                    .await
            }
            DaemonMessage::GetAddrInfoResponse(msg) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::AddrInfoRes(msg))
                    .await
            }
            DaemonMessage::Tcp(msg) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::AgentMirror(msg))
                    .await
            }
            DaemonMessage::TcpSteal(msg) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::AgentSteal(msg))
                    .await
            }
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    self.task_txs.agent.send(ClientMessage::ReadyForLogs).await;
                }
            }
            DaemonMessage::LogMessage(log) => match log.level {
                LogLevel::Error => tracing::error!("agent log: {}", log.message),
                LogLevel::Warn => tracing::warn!("agent log: {}", log.message),
            },
            other => {
                return Err(IntProxyError::UnexpectedAgentMessage(other));
            }
        }

        Ok(())
    }

    /// Routes a message from the layer to the correct background task.
    async fn handle_layer_message(&self, message: FromLayer) -> Result<(), IntProxyError> {
        let FromLayer {
            message_id,
            layer_id,
            message,
        } = message;

        match message {
            LayerToProxyMessage::File(req) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::FileReq(message_id, layer_id, req))
                    .await;
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::AddrInfoReq(message_id, layer_id, req))
                    .await
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::LayerConnect(
                        req, message_id, layer_id,
                    ))
                    .await
            }
            LayerToProxyMessage::Incoming(req) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::LayerRequest(
                        message_id, layer_id, req,
                    ))
                    .await
            }
            other => return Err(IntProxyError::UnexpectedLayerMessage(other)),
        }

        Ok(())
    }
}
