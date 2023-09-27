#![feature(async_fn_in_trait)]

use std::{sync::Arc, time::Duration};

use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage, FileRequest};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time,
};
use tokio_util::sync::CancellationToken;

use crate::{
    agent_conn::{AgentCommunicationFailed, AgentConnectInfo, AgentConnection},
    error::{IntProxyError, Result},
    layer_conn::LayerConnection,
    protocol::{LayerToProxyMessage, LocalMessage},
    proxies::{
        outgoing::{proxy::OutgoingProxy, DatagramHandler, StreamHandler},
        simple::SimpleProxy,
    },
};

pub mod agent_conn;
pub mod codec;
pub mod error;
mod layer_conn;
pub mod protocol;
mod proxies;
mod request_queue;

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

    pub async fn run(self: Arc<Self>, cancellation_token: CancellationToken) -> Result<()> {
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
                        let token_clone = cancellation_token.clone();

                        active_connections.spawn(async move {
                            let session = match ProxySession::new(&proxy_clone, stream, token_clone).await {
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
                    Err(err) if any_connection_accepted => {
                        tracing::error!("failed to accept connection: {err:#?}");
                        return Err(IntProxyError::AcceptFailed(err));
                    }
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
    ping: bool,
    cancellation_token: CancellationToken,
    simple_proxy: SimpleProxy,
    datagram_outgoing_proxy: OutgoingProxy<DatagramHandler>,
    stream_outgoing_proxy: OutgoingProxy<StreamHandler>,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    async fn new(
        intproxy: &IntProxy,
        conn: TcpStream,
        cancellation_token: CancellationToken,
    ) -> Result<Self> {
        let mut agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;
        agent_conn.ping_pong().await?;

        let layer_conn = LayerConnection::new(conn);

        let simple_proxy =
            SimpleProxy::new(layer_conn.sender().clone(), agent_conn.sender().clone());
        let datagram_outgoing_proxy =
            OutgoingProxy::new(agent_conn.sender().clone(), layer_conn.sender().clone());
        let stream_outgoing_proxy =
            OutgoingProxy::new(agent_conn.sender().clone(), layer_conn.sender().clone());

        Ok(Self {
            agent_conn,
            layer_conn,
            ping: false,
            cancellation_token,
            simple_proxy,
            datagram_outgoing_proxy,
            stream_outgoing_proxy,
        })
    }

    async fn serve_connection(mut self) -> Result<()> {
        let mut ping_interval = time::interval(Self::PING_INTERVAL);
        ping_interval.tick().await;

        loop {
            tokio::select! {
                layer_message = self.layer_conn.receive() => {
                    ping_interval.reset();

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

                _ = ping_interval.tick() => {
                    if !self.ping {
                        self.agent_conn.sender().send(ClientMessage::Ping).await?;
                        self.ping = true;
                    } else {
                        tracing::warn!("unmatched ping, timeout!");
                        break Err(AgentCommunicationFailed::UnmatchedPing.into());
                    }
                }
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        match message {
            DaemonMessage::Close(msg) => todo!(),
            DaemonMessage::Tcp(msg) => todo!(),
            DaemonMessage::TcpSteal(msg) => todo!(),
            DaemonMessage::TcpOutgoing(msg) => {
                self.stream_outgoing_proxy.handle_agent_message(msg).await
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.datagram_outgoing_proxy.handle_agent_message(msg).await
            }
            DaemonMessage::LogMessage(msg) => todo!(),
            DaemonMessage::File(msg) => self.simple_proxy.handle_response(msg).await,
            DaemonMessage::Pong => todo!(),
            DaemonMessage::GetEnvVarsResponse(msg) => todo!(),
            DaemonMessage::GetAddrInfoResponse(msg) => self.simple_proxy.handle_response(msg).await,
            DaemonMessage::PauseTarget(msg) => todo!(),
            DaemonMessage::SwitchProtocolVersionResponse(msg) => todo!(),
        }
    }

    async fn handle_layer_message(
        &mut self,
        message: LocalMessage<LayerToProxyMessage>,
    ) -> Result<()> {
        match message.inner {
            LayerToProxyMessage::InitSession(_) => todo!(),
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
            LayerToProxyMessage::ConnectUdpOutgoing(req) => {
                self.datagram_outgoing_proxy
                    .handle_layer_connect_request(req, message.message_id)
                    .await
            }
            LayerToProxyMessage::ConnectTcpOutgoing(req) => {
                self.stream_outgoing_proxy
                    .handle_layer_connect_request(req, message.message_id)
                    .await
            }
        }
    }
}
