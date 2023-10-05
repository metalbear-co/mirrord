#![feature(lazy_cell)]
#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use agent_conn::AgentCommunicationError;
use layer_conn::LayerConnection;
use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use ping_pong::AgentSentMessage;
use protocol::{ProxyToLayerMessage, NOT_A_RESPONSE};
use proxies::{
    outgoing::{OutgoingProxy, OutgoingProxyIn},
    simple::{SimpleProxy, SimpleProxyIn},
};
use semver::VersionReq;
use task_manager::ManagedTask;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time,
};

use crate::{
    agent_conn::{AgentConnectInfo, AgentConnection},
    error::{IntProxyError, Result},
    ping_pong::PingPong,
    protocol::{LayerToProxyMessage, LocalMessage},
    task_manager::TaskMessageOut,
};

pub mod agent_conn;
pub mod codec;
pub mod error;
mod layer_conn;
mod ping_pong;
pub mod protocol;
mod proxies;
mod request_queue;
mod task_manager;

/// Minimal [`mirrord_protocol`] version that allows [`ClientMessage::ReadyForLogs`] message.
pub static CLIENT_READY_FOR_LOGS: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.3.1".parse().expect("Bad Identifier"));

pub struct IntProxy {
    config: LayerConfig,
    agent_connect_info: Option<AgentConnectInfo>,
    listener: TcpListener,
}

impl IntProxy {
    pub fn new(
        config: LayerConfig,
        agent_connect_info: Option<AgentConnectInfo>,
        listener: TcpListener,
    ) -> Self {
        Self {
            config,
            agent_connect_info,
            listener,
        }
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let first_timeout = Duration::from_secs(self.config.internal_proxy.start_idle_timeout);
        let consecutive_timeout = Duration::from_secs(self.config.internal_proxy.idle_timeout);

        let mut any_connection_accepted = false;

        let mut active_connections = JoinSet::new();

        loop {
            tokio::select! {
                res = self.listener.accept() => match res {
                    Ok((stream, peer)) => {
                        tracing::trace!("accepted connection from {peer}");

                        any_connection_accepted = true;

                        let proxy_clone = self.clone();
                        active_connections.spawn(async move {
                            let session = match ProxySession::new(&proxy_clone, stream).await {
                                Ok(session) => session,
                                Err(err) => {
                                    tracing::error!("failed to create a new session for {peer}: {err:?}");
                                    return;
                                }
                            };

                            if let Err(err) = session.serve_connection().await {
                                tracing::error!("an error occurred when handling connection from {peer}: {err:?}");
                            }
                        });
                    },
                    Err(err) => {
                        tracing::error!("failed to accept first connection: {err:#?}");
                        return Err(IntProxyError::AcceptFailed(err));
                    }
                },

                _ = active_connections.join_next(), if !active_connections.is_empty() => {},

                _ = time::sleep(first_timeout), if !any_connection_accepted => {
                    return Err(IntProxyError::FirstConnectionTimeout);
                },

                _ = time::sleep(consecutive_timeout), if any_connection_accepted => {
                    if active_connections.is_empty() {
                        tracing::trace!("intproxy timeout, no active connections. Exiting.");
                        break Ok(());
                    }

                    tracing::trace!(
                        "intproxy {} sec tick, {} active_connection(s).",
                        self.config.internal_proxy.idle_timeout,
                        active_connections.len(),
                    );
                },
            }
        }
    }
}

struct ProxySession {
    agent_conn: AgentConnection,
    layer_conn: LayerConnection,

    simple_proxy: ManagedTask<SimpleProxy>,
    outgoing_proxy: ManagedTask<OutgoingProxy>,
    ping_pong: ManagedTask<PingPong>,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    const CHANNEL_SIZE: usize = 512;

    async fn new(intproxy: &IntProxy, conn: TcpStream) -> Result<Self> {
        let mut agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;
        agent_conn.ping_pong().await?;

        let layer_conn = LayerConnection::new(conn, Self::CHANNEL_SIZE);

        let simple_proxy = ManagedTask::spawn(SimpleProxy::default(), Self::CHANNEL_SIZE);
        let outgoing_proxy = ManagedTask::spawn(OutgoingProxy::default(), Self::CHANNEL_SIZE);
        let ping_pong =
            ManagedTask::spawn(PingPong::new(Self::PING_INTERVAL).await, Self::CHANNEL_SIZE);

        Ok(Self {
            agent_conn,
            layer_conn,

            simple_proxy,
            outgoing_proxy,
            ping_pong,
        })
    }

    async fn serve_connection(mut self) -> Result<()> {
        self.agent_conn
            .sender()
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await?;

        loop {
            tokio::select! {
                biased;

                res = self.simple_proxy.receive() => match res {
                    TaskMessageOut::Raw { .. } => {},
                    TaskMessageOut::Result { .. } => {

                    }
                },

                res = self.outgoing_proxy.receive() => {

                },

                res = self.ping_pong.receive() => {

                },

                msg = self.layer_conn.receive() => match msg {
                    Some(msg) => self.handle_layer_message(msg).await,
                    None => {
                        tracing::trace!("layer connection closed");
                        break Ok(());
                    }
                },

                msg = self.agent_conn.receive() => self.handle_agent_message(msg?).await?,
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        self.ping_pong
            .send(AgentSentMessage {
                pong: matches!(message, DaemonMessage::Pong),
            })
            .await;

        match message {
            DaemonMessage::Pong => {}
            DaemonMessage::Close(reason) => {
                return Err(IntProxyError::AgentClosedConnection(reason))
            }
            DaemonMessage::TcpOutgoing(msg) => {
                self.outgoing_proxy
                    .send(OutgoingProxyIn::AgentStreams(msg))
                    .await
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.outgoing_proxy
                    .send(OutgoingProxyIn::AgentDatagrams(msg))
                    .await
            }
            DaemonMessage::File(msg) => self.simple_proxy.send(SimpleProxyIn::FileRes(msg)).await,
            DaemonMessage::GetAddrInfoResponse(msg) => {
                self.simple_proxy
                    .send(SimpleProxyIn::AddrInfoRes(msg))
                    .await
            }
            DaemonMessage::Tcp(..) => todo!(),
            DaemonMessage::TcpSteal(..) => todo!(),
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    self.agent_conn
                        .sender()
                        .send(ClientMessage::ReadyForLogs)
                        .await?;
                }
            }
            DaemonMessage::LogMessage(log) => {
                self.layer_conn
                    .sender()
                    .send(LocalMessage {
                        message_id: NOT_A_RESPONSE,
                        inner: ProxyToLayerMessage::AgentLog(log),
                    })
                    .await?;
            }
            other => {
                return Err(IntProxyError::AgentCommunicationError(
                    AgentCommunicationError::UnexpectedMessage(other),
                ))
            }
        };

        Ok(())
    }

    async fn handle_layer_message(&mut self, message: LocalMessage<LayerToProxyMessage>) {
        match message.inner {
            LayerToProxyMessage::NewSession(..) => todo!(),
            LayerToProxyMessage::File(req) => {
                self.simple_proxy
                    .send(SimpleProxyIn::FileReq(LocalMessage {
                        inner: req,
                        message_id: message.message_id,
                    }))
                    .await
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.simple_proxy
                    .send(SimpleProxyIn::AddrInfoReq(LocalMessage {
                        message_id: message.message_id,
                        inner: req,
                    }))
                    .await;
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.outgoing_proxy
                    .send(OutgoingProxyIn::ConnectRequest(LocalMessage {
                        message_id: message.message_id,
                        inner: req,
                    }))
                    .await;
            }
            LayerToProxyMessage::Incoming(..) => todo!(),
        }
    }
}
