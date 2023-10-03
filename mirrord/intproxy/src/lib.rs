#![feature(lazy_cell)]
#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use ping_pong::PingPongMessage;
use protocol::{ProxyToLayerMessage, NOT_A_RESPONSE};
use semver::VersionReq;
use system::{ComponentRef, System};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc::Receiver,
    task::JoinSet,
    time,
};

use crate::{
    agent_conn::{AgentCommunicationFailed, AgentConnectInfo, AgentConnection},
    error::{IntProxyError, Result},
    layer_conn::LayerConnector,
    ping_pong::PingPong,
    protocol::{LayerToProxyMessage, LocalMessage},
    proxies::{outgoing::OutgoingProxy, simple::SimpleProxy},
    system::ComponentError,
};

pub mod agent_conn;
pub mod codec;
pub mod error;
mod layer_conn;
mod ping_pong;
pub mod protocol;
mod proxies;
mod request_queue;
mod system;

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
    layer_rx: Receiver<LocalMessage<LayerToProxyMessage>>,

    system: System<&'static str, IntProxyError>,

    layer_connector_ref: ComponentRef<LayerConnector>,
    simple_proxy_ref: ComponentRef<SimpleProxy>,
    outgoing_proxy_ref: ComponentRef<OutgoingProxy>,
    ping_pong_ref: ComponentRef<PingPong>,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    async fn new(intproxy: &IntProxy, conn: TcpStream) -> Result<Self> {
        let mut agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;
        agent_conn.ping_pong().await?;

        let mut system = System::default();

        let (layer_connector, layer_rx) = LayerConnector::new(conn);
        let layer_connector_ref = system.register(layer_connector);
        let simple_proxy_ref = system.register(SimpleProxy::new(
            layer_connector_ref.clone(),
            agent_conn.sender().clone(),
        ));
        let outgoing_proxy_ref = system.register(OutgoingProxy::new(
            layer_connector_ref.clone(),
            agent_conn.sender().clone(),
        ));

        let ping_pong_ref =
            system.register(PingPong::new(Self::PING_INTERVAL, agent_conn.sender().clone()).await);

        Ok(Self {
            agent_conn,
            layer_rx,

            system,
            layer_connector_ref,
            simple_proxy_ref,
            outgoing_proxy_ref,
            ping_pong_ref,
        })
    }

    async fn serve_connection(mut self) -> Result<()> {
        self.agent_conn
            .sender()
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        loop {
            tokio::select! {
                msg = self.layer_rx.recv() => match msg {
                    Some(msg) => self.handle_layer_message(msg).await,
                    None => {
                        tracing::trace!("layer connection closed");
                        break Ok(());
                    }
                },

                msg = self.agent_conn.receive() => {
                    let Some(message) = msg else {
                        tracing::error!("agent connection closed");
                        break Err(IntProxyError::AgentClosedConnection("no reason specified".into()));
                    };

                    self.handle_agent_message(message).await?;
                },

                Some(res) = self.system.next() => match res {
                    Ok(id) => tracing::info!("component {id} finished"),
                    Err(ComponentError::Panic(id)) => {
                        let err = IntProxyError::ComponentError(ComponentError::Panic(id));
                        tracing::error!("{err}");
                        return Err(err);
                    },
                    Err(ComponentError::Error(id, e)) => {
                        let err = IntProxyError::ComponentError(ComponentError::Error(id, e.into()));
                        tracing::error!("{err}");
                        return Err(err);
                    }
                }
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        let ping_pong_message = match &message {
            DaemonMessage::Pong => PingPongMessage::Pong,
            _ => PingPongMessage::NotPong,
        };
        self.ping_pong_ref.send(ping_pong_message).await;

        match message {
            DaemonMessage::Pong => Ok(()),
            DaemonMessage::Close(reason) => Err(IntProxyError::AgentClosedConnection(reason)),
            DaemonMessage::TcpOutgoing(msg) => {
                self.outgoing_proxy_ref.send(msg).await;
                Ok(())
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.outgoing_proxy_ref.send(msg).await;
                Ok(())
            }
            DaemonMessage::File(msg) => {
                self.simple_proxy_ref.send(msg).await;
                Ok(())
            }
            DaemonMessage::GetAddrInfoResponse(msg) => {
                self.simple_proxy_ref.send(msg).await;
                Ok(())
            }
            DaemonMessage::Tcp(..) => todo!(),
            DaemonMessage::TcpSteal(..) => todo!(),
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    self.agent_conn
                        .sender()
                        .send(ClientMessage::ReadyForLogs)
                        .await;
                }

                Ok(())
            }
            DaemonMessage::LogMessage(log) => {
                self.layer_connector_ref
                    .send(LocalMessage {
                        message_id: NOT_A_RESPONSE,
                        inner: ProxyToLayerMessage::AgentLog(log),
                    })
                    .await;
                Ok(())
            }
            other => Err(IntProxyError::AgentCommunicationFailed(
                AgentCommunicationFailed::UnexpectedMessage(other),
            )),
        }
    }

    async fn handle_layer_message(&mut self, message: LocalMessage<LayerToProxyMessage>) {
        match message.inner {
            LayerToProxyMessage::NewSession(..) => todo!(),
            LayerToProxyMessage::File(req) => {
                self.simple_proxy_ref
                    .send(LocalMessage {
                        inner: req,
                        message_id: message.message_id,
                    })
                    .await
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.simple_proxy_ref
                    .send(LocalMessage {
                        inner: req,
                        message_id: message.message_id,
                    })
                    .await
            }
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.outgoing_proxy_ref
                    .send(LocalMessage {
                        message_id: message.message_id,
                        inner: req,
                    })
                    .await
            }
            LayerToProxyMessage::Incoming(..) => todo!(),
        }
    }
}
