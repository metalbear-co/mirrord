#![feature(lazy_cell)]

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage, FileRequest, LogLevel};
use semver::VersionReq;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time,
};

use crate::{
    agent_conn::{AgentCommunicationFailed, AgentConnectInfo, AgentConnection},
    error::{IntProxyError, Result},
    layer_conn::LayerConnection,
    ping_pong::PingPong,
    protocol::{LayerToProxyMessage, LocalMessage},
    proxies::{incoming::IncomingProxy, outgoing::proxy::OutgoingProxy, simple::SimpleProxy},
};

pub mod agent_conn;
pub mod codec;
pub mod error;
mod layer_conn;
mod ping_pong;
pub mod protocol;
mod proxies;
mod request_queue;

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
    simple_proxy: SimpleProxy,
    outgoing_proxy: OutgoingProxy,
    incoming_proxy: IncomingProxy,
    ping_pong: PingPong,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    async fn new(intproxy: &IntProxy, conn: TcpStream) -> Result<Self> {
        let mut agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;
        agent_conn.ping_pong().await?;

        let layer_conn = LayerConnection::new(conn);

        let simple_proxy =
            SimpleProxy::new(layer_conn.sender().clone(), agent_conn.sender().clone());
        let outgoing_proxy =
            OutgoingProxy::new(agent_conn.sender().clone(), layer_conn.sender().clone());
        let incoming_proxy = IncomingProxy::new(
            &intproxy.config.feature.network.incoming,
            agent_conn.sender().clone(),
            layer_conn.sender().clone(),
        );

        let ping_pong = PingPong::new(Self::PING_INTERVAL).await;

        Ok(Self {
            agent_conn,
            layer_conn,
            simple_proxy,
            outgoing_proxy,
            incoming_proxy,
            ping_pong,
        })
    }

    async fn serve_connection(mut self) -> Result<()> {
        if let Err(err) = self
            .agent_conn
            .sender()
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
        {
            tracing::error!("failed to switch protocol version: {err:?}");
        }

        loop {
            tokio::select! {
                layer_message = self.layer_conn.receive() => {
                    let Some(message) = layer_message else {
                        tracing::trace!("layer connection closed");
                        break Ok(());
                    };

                    self.handle_layer_message(message).await?;
                },

                agent_message = self.agent_conn.receive() => {
                    let Some(message) = agent_message else {
                        tracing::trace!("agent connection closed");
                        break Ok(());
                    };

                    self.handle_agent_message(message).await?;
                },

                res = self.ping_pong.ready() => {
                    res?;
                    self.agent_conn.sender().send(ClientMessage::Ping).await?;
                    self.ping_pong.ping_sent();
                }
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        self.ping_pong.inspect_agent_message(&message)?;

        match message {
            DaemonMessage::Pong => Ok(()),
            DaemonMessage::Close(reason) => Err(IntProxyError::AgentClosedConnection(reason)),
            DaemonMessage::TcpOutgoing(msg) => {
                self.outgoing_proxy.handle_agent_tcp_message(msg).await
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.outgoing_proxy.handle_agent_udp_message(msg).await
            }
            DaemonMessage::File(msg) => self.simple_proxy.handle_response(msg).await,
            DaemonMessage::GetAddrInfoResponse(msg) => self.simple_proxy.handle_response(msg).await,
            DaemonMessage::Tcp(msg) => self.incoming_proxy.handle_agent_message(msg).await,
            DaemonMessage::TcpSteal(msg) => self.incoming_proxy.handle_agent_message(msg).await,
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    if let Err(e) = self
                        .agent_conn
                        .sender()
                        .send(ClientMessage::ReadyForLogs)
                        .await
                    {
                        tracing::error!("unable to enable logs from the agent: {e}");
                    }
                }

                Ok(())
            }
            DaemonMessage::LogMessage(log) => {
                // TODO: we don't have any thread in the layer to handle this
                match log.level {
                    LogLevel::Error => tracing::error!("agent error: {}", log.message),
                    LogLevel::Warn => tracing::warn!("agent warning: {}", log.message),
                }

                Ok(())
            }
            other => Err(IntProxyError::AgentCommunicationFailed(
                AgentCommunicationFailed::UnexpectedMessage(other),
            )),
        }
    }

    async fn handle_layer_message(
        &mut self,
        message: LocalMessage<LayerToProxyMessage>,
    ) -> Result<()> {
        match message.inner {
            LayerToProxyMessage::NewSession(..) => todo!(),
            LayerToProxyMessage::File(req) => match req {
                req @ (FileRequest::CloseDir(..) | FileRequest::Close(..)) => self
                    .agent_conn
                    .sender()
                    .send(ClientMessage::FileRequest(req))
                    .await
                    .map_err(Into::into),
                req => {
                    self.simple_proxy
                        .handle_request(req, message.message_id)
                        .await
                }
            },
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.simple_proxy
                    .handle_request(req, message.message_id)
                    .await
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.outgoing_proxy
                    .handle_layer_connect_request(req, message.message_id)
                    .await
            }
            LayerToProxyMessage::Incoming(req) => {
                self.incoming_proxy
                    .handle_layer_request(req, message.message_id)
                    .await
            }
        }
    }
}
