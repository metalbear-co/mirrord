use std::{sync::Arc, time::Duration};

use codec::{AsyncReceiver, AsyncSender};
use mirrord_config::LayerConfig;
use mirrord_kube::api::{
    kubernetes::{AgentKubernetesConnectInfo, KubernetesAPI},
    wrap_raw_connection, AgentManagment,
};
use mirrord_operator::client::{OperatorApi, OperatorSessionInformation};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use protocol::{LayerToProxyMessage, ProxyToLayerMessage};
use serde::{Deserialize, Serialize};
use tokio::{
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    sync::mpsc::{Receiver, Sender},
    task::JoinSet,
    time,
};

use crate::error::{IntProxyError, Result};

pub mod codec;
pub mod error;
pub mod protocol;

#[derive(Debug, Serialize, Deserialize)]
pub enum AgentConnectInfo {
    /// Connect to the agent through the operator.
    Operator(OperatorSessionInformation),
    /// Connect directly to the agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
}

pub struct IntProxy {
    config: LayerConfig,
    agent_connect_info: Option<AgentConnectInfo>,
    listener: TcpListener,
}

struct AgentConnection {
    sender: Sender<ClientMessage>,
    receiver: Receiver<DaemonMessage>,
}

impl AgentConnection {
    async fn send(&self, message: ClientMessage) -> Result<()> {
        self.sender
            .send(message)
            .await
            .map_err(|_| IntProxyError::AgentCommunicationFailed)
    }

    async fn receive(&mut self) -> Option<DaemonMessage> {
        self.receiver.recv().await
    }

    async fn ping(&mut self) -> Result<()> {
        self.send(ClientMessage::Ping).await?;
        self.receive()
            .await
            .ok_or(IntProxyError::AgentCommunicationFailed)?;

        Ok(())
    }
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

    async fn connect_to_agent(&self) -> Result<AgentConnection> {
        let (sender, receiver) = match self.agent_connect_info.as_ref() {
            Some(AgentConnectInfo::Operator(operator_session_information)) => {
                OperatorApi::connect(&self.config, operator_session_information, None)
                    .await
                    .map_err(IntProxyError::OperatorAgentConnectionFailed)?
            }
            Some(AgentConnectInfo::DirectKubernetes(connect_info)) => {
                let k8s_api = KubernetesAPI::create(&self.config)
                    .await
                    .map_err(IntProxyError::KubeApiAgentConnectionFailed)?;
                k8s_api
                    .create_connection(connect_info.clone())
                    .await
                    .map_err(IntProxyError::KubeApiAgentConnectionFailed)?
            }
            None => {
                if let Some(address) = &self.config.connect_tcp {
                    let stream = TcpStream::connect(address)
                        .await
                        .map_err(IntProxyError::RawAgentConnectionFailed)?;

                    wrap_raw_connection(stream)
                } else {
                    return Err(IntProxyError::NoConnectionMethod);
                }
            }
        };

        Ok(AgentConnection { sender, receiver })
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
                    Err(err) if any_connection_accepted => {
                        tracing::error!("failed to accept connection: {err:#?}");
                        return Err(IntProxyError::AcceptFailed(err));
                    }
                    Err(err) => {
                        tracing::error!("failed to accept first connection: {err:#?}");
                        return Err(IntProxyError::FirstAcceptFailed(err));
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
    layer_sender: AsyncSender<ProxyToLayerMessage, OwnedWriteHalf>,
    layer_receiver: AsyncReceiver<LayerToProxyMessage, OwnedReadHalf>,
    ping: bool,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    async fn new(intproxy: &IntProxy, conn: TcpStream) -> Result<Self> {
        let mut agent_conn = intproxy.connect_to_agent().await?;
        agent_conn.ping().await?;

        let (layer_sender, layer_receiver) =
            codec::make_async_framed::<ProxyToLayerMessage, LayerToProxyMessage>(conn);

        Ok(Self {
            agent_conn,
            layer_sender,
            layer_receiver,
            ping: false,
        })
    }

    async fn serve_connection(mut self) -> Result<()> {
        let mut ping_interval = time::interval(Self::PING_INTERVAL);
        ping_interval.tick().await;

        loop {
            tokio::select! {
                layer_message = self.layer_receiver.receive() => {
                    ping_interval.reset();

                    let Some(message) = layer_message? else {
                        tracing::trace!("layer connection closed");
                        break Ok(());
                    };

                    self.handle_layer_message(message).await?;
                },

                agent_message = self.agent_conn.receive() => {
                    match agent_message {
                        Some(agent_message) => self.handle_agent_message(agent_message).await?,
                        None => {
                            tracing::trace!("agent connection closed");
                            break Ok(());
                        }
                    }
                },

                _ = ping_interval.tick() => {
                    if !self.ping {
                        self.agent_conn.send(ClientMessage::Ping).await?;
                        self.ping = true;
                    } else {
                        tracing::warn!("Unmatched ping, timeout!");
                        break Err(IntProxyError::AgentCommunicationFailed);
                    }
                }
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        match message {
            DaemonMessage::Pong => {
                self.ping = false;
            }
            agent_message => {
                self.layer_sender
                    .send(&ProxyToLayerMessage::DaemonMessage(agent_message))
                    .await?
            }
        }

        Ok(())
    }

    async fn handle_layer_message(&mut self, message: LayerToProxyMessage) -> Result<()> {
        match message {
            LayerToProxyMessage::ClientMessage(message) => self.agent_conn.send(message).await,
        }
    }
}
