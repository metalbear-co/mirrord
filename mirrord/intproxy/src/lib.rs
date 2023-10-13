#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

use std::{collections::HashMap, fmt, net::SocketAddr, time::Duration};

use agent_conn::AgentConnection;
use background_tasks::{BackgroundTasks, TaskSender, TaskUpdate};
use codec::AsyncEncoder;
use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage, CLIENT_READY_FOR_LOGS};
use ping_pong::{AgentMessageNotification, PingPong};
use protocol::{LayerId, ProxyToLayerMessage, NOT_A_RESPONSE};
use proxies::{
    incoming::{IncomingProxy, IncomingProxyMessage},
    outgoing::{OutgoingProxy, OutgoingProxyMessage},
    simple::{SimpleProxy, SimpleProxyMessage},
};
use tokio::{
    net::{TcpListener, TcpStream},
    time,
};

use crate::{
    agent_conn::AgentConnectInfo,
    background_tasks::TaskError,
    codec::AsyncDecoder,
    error::{IntProxyError, SessionInitError},
    layer_conn::LayerConnection,
    protocol::{LayerToProxyMessage, LocalMessage},
};

pub mod agent_conn;
mod background_tasks;
pub mod codec;
pub mod error;
mod layer_conn;
mod ping_pong;
pub mod protocol;
mod proxies;
mod request_queue;

impl LayerId {
    fn bump(&mut self) -> LayerId {
        let res = LayerId(self.0);
        self.0 += 1;
        res
    }
}

/// Enumerated ids of main [`BackgroundTask`](crate::background_tasks::BackgroundTask)s used by
/// [`IntProxy`].
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
    LayerConnection(LayerId),
}

impl fmt::Display for MainTaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SimpleProxy => f.write_str("SIMPLE_PROXY"),
            Self::OutgoingProxy => f.write_str("OUTGOING_PROXY"),
            Self::PingPong => f.write_str("PING_PONG"),
            Self::AgentConnection => f.write_str("AGENT_CONNECTION"),
            Self::LayerConnection(id) => write!(f, "LAYER_CONNECTION {}", id.0),
            Self::IncomingProxy => f.write_str("INCOMING_PROXY"),
        }
    }
}

/// Messages sent back to the [`IntProxy`] from the main background tasks. See [`MainTaskId`].
#[derive(Debug)]
pub enum ProxyMessage {
    /// Message to be sent to the agent. Handled by [`AgentConnection`].
    ToAgent(ClientMessage),
    /// Message to be sent to the layer. Handled by [`LayerConnection`].
    ToLayer(LocalMessage<ProxyToLayerMessage>, LayerId),
    /// Message received from the agent. Handled by one of the main background tasks.
    FromAgent(DaemonMessage),
    /// Message received from the layer. Handled by one of the main background tasks.
    FromLayer(LocalMessage<LayerToProxyMessage>, LayerId),
}

/// [`TaskSender`]s for main background tasks. See [`MainTaskId`].
struct TaskTxs {
    /// For [`LayerConnection`]s.
    layers: HashMap<LayerId, TaskSender<LocalMessage<ProxyToLayerMessage>>>,
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

pub struct IntProxy {
    listener: TcpListener,
    next_layer_id: LayerId,
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
    task_txs: TaskTxs,
}

impl IntProxy {
    const CHANNEL_SIZE: usize = 512;
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    pub async fn new(
        config: &LayerConfig,
        agent_connect_info: Option<&AgentConnectInfo>,
        listener: TcpListener,
    ) -> Result<Self, IntProxyError> {
        let agent_conn = AgentConnection::new(config, agent_connect_info).await?;

        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
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
            listener,
            next_layer_id: LayerId(0),
            background_tasks,
            task_txs: TaskTxs {
                layers: Default::default(),
                agent,
                simple,
                outgoing,
                incoming,
                ping_pong,
            },
        })
    }

    #[tracing::instrument(level = "trace", skip(self, stream), ret)]
    async fn handle_new_layer(
        &mut self,
        stream: TcpStream,
        peer: SocketAddr,
    ) -> Result<(), SessionInitError> {
        let mut decoder: AsyncDecoder<LocalMessage<LayerToProxyMessage>, _> =
            AsyncDecoder::new(stream);
        let msg = decoder.receive().await?;

        let Some(msg) = msg else {
            tracing::trace!("layer peer {peer} closed connection without any message");
            return Ok(());
        };

        let new_session_id = self.next_layer_id.bump();

        let mut encoder: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, _> =
            AsyncEncoder::new(decoder.into_inner());
        encoder
            .send(&LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(new_session_id),
            })
            .await?;
        encoder.flush().await?;

        let stream = encoder.into_inner();

        let tx = self.background_tasks.register(
            LayerConnection::new(stream, new_session_id),
            MainTaskId::LayerConnection(new_session_id),
            Self::CHANNEL_SIZE,
        );
        self.task_txs.layers.insert(new_session_id, tx);

        Ok(())
    }

    pub async fn run(
        mut self,
        first_timeout: Duration,
        consecutive_timeout: Duration,
    ) -> Result<(), IntProxyError> {
        let mut any_connection_accepted = false;

        loop {
            tokio::select! {
                res = self.listener.accept() => match res {
                    Ok((stream, peer)) => {
                        self.handle_new_layer(stream, peer).await.map_err(|e| IntProxyError::SessionInitError(peer, e))?;
                        any_connection_accepted = true;
                    },
                    Err(e) => return Err(IntProxyError::AcceptFailed(e)),
                },

                Some((task_id, task_update)) = self.background_tasks.next() => {
                    self.handle_task_update(task_id, task_update).await?;
                }

                _ = time::sleep(first_timeout), if !any_connection_accepted => {
                    if !any_connection_accepted {
                        return Err(IntProxyError::FirstConnectionTimeout);
                    }
                },

                _ = time::sleep(consecutive_timeout), if any_connection_accepted && self.task_txs.layers.is_empty() => {
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
    async fn handle(&mut self, msg: ProxyMessage) -> Result<(), IntProxyError> {
        match msg {
            ProxyMessage::FromAgent(msg) => self.handle_agent_message(msg).await?,
            ProxyMessage::FromLayer(msg, session_id) => {
                self.handle_layer_message(msg, session_id).await?
            }
            ProxyMessage::ToAgent(msg) => self.task_txs.agent.send(msg).await,
            ProxyMessage::ToLayer(msg, id) => {
                self.task_txs.layers.get(&id).expect("TODO").send(msg).await
            }
        }

        Ok(())
    }

    async fn handle_task_update(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, IntProxyError>,
    ) -> Result<(), IntProxyError> {
        match (task_id, update) {
            (MainTaskId::LayerConnection(LayerId(id)), TaskUpdate::Finished(Ok(()))) => {
                tracing::trace!("layer connection {id} closed");
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
            DaemonMessage::LogMessage(log) => {
                if let Some(sender) = self.task_txs.layers.values().next() {
                    sender
                        .send(LocalMessage {
                            message_id: NOT_A_RESPONSE,
                            inner: ProxyToLayerMessage::AgentLog(log),
                        })
                        .await;
                }
            }
            other => {
                return Err(IntProxyError::UnexpectedAgentMessage(other));
            }
        }

        Ok(())
    }

    /// Routes a message from the layer to the correct background task.
    async fn handle_layer_message(
        &self,
        message: LocalMessage<LayerToProxyMessage>,
        session_id: LayerId,
    ) -> Result<(), IntProxyError> {
        match message.inner {
            LayerToProxyMessage::File(req) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::FileReq(
                        message.message_id,
                        session_id,
                        req,
                    ))
                    .await;
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::AddrInfoReq(
                        message.message_id,
                        session_id,
                        req,
                    ))
                    .await
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::LayerConnect(
                        req,
                        message.message_id,
                        session_id,
                    ))
                    .await
            }
            LayerToProxyMessage::Incoming(req) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::LayerRequest(
                        message.message_id,
                        session_id,
                        req,
                    ))
                    .await
            }
            other => return Err(IntProxyError::UnexpectedLayerMessage(other)),
        }

        Ok(())
    }
}
