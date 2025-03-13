//! Implementation of `proxy <-> agent` connection through [`mpsc`] channels created in different
//! mirrord crates.

use std::{fmt, io, net::SocketAddr, ops::ControlFlow, path::PathBuf};

use mirrord_analytics::{NullReporter, Reporter};
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{kubernetes::AgentKubernetesConnectInfo, wrap_raw_connection},
    error::KubeApiError,
};
use mirrord_operator::client::{error::OperatorApiError, OperatorApi, OperatorSession};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use tls::ConnectionTlsError;
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::mpsc::{Receiver, Sender},
};
use tokio_retry::{
    strategy::{jitter, ExponentialBackoff},
    Retry,
};
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus, RestartableBackgroundTask},
    main_tasks::{ConnectionRefresh, ProxyMessage},
};

mod portforward;
mod tls;

/// Errors that can occur when the internal proxy tries to establish a connection with the agent.
#[derive(Error, Debug)]
pub enum AgentConnectionError {
    /// IO failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// mirrord operator API failed.
    #[error(transparent)]
    Operator(#[from] OperatorApiError),
    /// mirrord kube API failed.
    #[error(transparent)]
    Kube(#[from] KubeApiError),
    /// Making a TLS connection to the mirrord-extproxy failed.
    #[error("failed to prepare TLS communication with mirrord-extproxy: {0}")]
    Tls(#[from] ConnectionTlsError),
}

/// Directive for the proxy on how to connect to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentConnectInfo {
    /// Connect to agent through `mirrord extproxy`.
    ExternalProxy {
        proxy_addr: SocketAddr,
        tls_pem: Option<PathBuf>,
    },
    /// Connect to the agent through the operator.
    Operator(OperatorSession),
    /// Connect directly to the agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyTlsSettings {
    intproxy_cert: PathBuf,
    intproxy_key: PathBuf,
    extproxy_cert: PathBuf,
}

#[derive(Default)]
pub enum ReconnectFlow {
    ConnectInfo {
        config: LayerConfig,
        connect_info: AgentConnectInfo,
    },

    #[default]
    Break,
}

impl fmt::Debug for ReconnectFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectInfo { connect_info, .. } => {
                f.debug_tuple("ConnectInfo").field(connect_info).finish()
            }
            Self::Break => f.write_str("Break"),
        }
    }
}

/// Handles logic of the `proxy <-> agent` connection as a [`BackgroundTask`].
///
/// # Note
/// The raw IO is managed in a separate [`tokio::task`] created in a different mirrord crate.
/// This differs from the [`LayerConnection`](crate::layer_conn::LayerConnection) implementation,
/// but this logic was already implemented elsewhere. This struct simply wraps the
/// [`mpsc`] channels returned from other functions and implements the [`BackgroundTask`] trait.
pub struct AgentConnection {
    pub agent_tx: Sender<ClientMessage>,
    pub agent_rx: Receiver<DaemonMessage>,
    pub reconnect: ReconnectFlow,
}

impl AgentConnection {
    /// Creates a new agent connection based on the provided [`LayerConfig`] and optional
    /// [`AgentConnectInfo`].
    #[tracing::instrument(level = Level::INFO, skip(config, analytics), ret, err)]
    pub async fn new<R: Reporter>(
        config: &LayerConfig,
        connect_info: AgentConnectInfo,
        analytics: &mut R,
    ) -> Result<Self, AgentConnectionError> {
        let (agent_tx, agent_rx, reconnect) = match connect_info {
            AgentConnectInfo::Operator(session) => {
                let connection =
                    OperatorApi::connect_in_existing_session(config, session.clone(), analytics)
                        .await?;
                (
                    connection.tx,
                    connection.rx,
                    if session.allow_reconnect {
                        ReconnectFlow::ConnectInfo {
                            config: config.clone(),
                            connect_info: AgentConnectInfo::Operator(session),
                        }
                    } else {
                        ReconnectFlow::default()
                    },
                )
            }

            AgentConnectInfo::ExternalProxy {
                proxy_addr,
                tls_pem,
            } => {
                let socket = TcpSocket::new_v4()?;
                socket.set_keepalive(true)?;
                socket.set_nodelay(true)?;

                let stream = socket.connect(proxy_addr).await?;

                let (tx, rx) = match tls_pem {
                    Some(tls_pem) => tls::wrap_raw_connection(stream, tls_pem.as_path()).await?,
                    None => wrap_raw_connection(stream),
                };

                (tx, rx, ReconnectFlow::default())
            }

            AgentConnectInfo::DirectKubernetes(connect_info) => {
                let (tx, rx) = portforward::create_connection(config, connect_info.clone()).await?;
                (tx, rx, ReconnectFlow::default())
            }
        };

        Ok(Self {
            agent_tx,
            agent_rx,
            reconnect,
        })
    }

    pub async fn new_for_raw_address(address: SocketAddr) -> Result<Self, AgentConnectionError> {
        let stream = TcpStream::connect(address).await?;
        let (agent_tx, agent_rx) = wrap_raw_connection(stream);

        Ok(Self {
            agent_tx,
            agent_rx,
            reconnect: ReconnectFlow::Break,
        })
    }

    #[tracing::instrument(level = Level::TRACE, name = "send_message_to_agent", skip(self), ret, err(level = Level::TRACE))]
    async fn send(&self, msg: ClientMessage) -> Result<(), AgentChannelError> {
        self.agent_tx.send(msg).await.map_err(|_| AgentChannelError)
    }
}

impl fmt::Debug for AgentConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConnection")
            .field("reconnect", &self.reconnect)
            .finish()
    }
}

/// This error occurs when the [`AgentConnection`] fails to communicate with the inner
/// [`tokio::task`], which handles raw IO. The original (e.g. some IO error) is not available.
#[derive(Error, Debug)]
#[error("agent unexpectedly closed connection")]
pub struct AgentChannelError;

impl BackgroundTask for AgentConnection {
    type Error = AgentChannelError;
    type MessageIn = ClientMessage;
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::INFO, name = "agent_connection_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    },
                    Some(msg) => {
                        if let Err(error) = self.send(msg).await {
                            tracing::error!(%error, "failed to send message to the agent");
                            break Err(error);
                        }
                    }
                },

                msg = self.agent_rx.recv() => match msg {
                    None => {
                        tracing::error!("failed to receive message from the agent, inner task down");
                        break Err(AgentChannelError);
                    }
                    Some(msg) => message_bus.send(ProxyMessage::FromAgent(msg)).await,
                },
            }
        }
    }
}

impl RestartableBackgroundTask for AgentConnection {
    #[tracing::instrument(level = Level::INFO, skip(self, message_bus), ret)]
    async fn restart(
        &mut self,
        error: Self::Error,
        message_bus: &mut MessageBus<Self>,
    ) -> ControlFlow<Self::Error> {
        match &self.reconnect {
            ReconnectFlow::Break => ControlFlow::Break(error),
            ReconnectFlow::ConnectInfo {
                config,
                connect_info,
            } => {
                message_bus
                    .send(ProxyMessage::ConnectionRefresh(ConnectionRefresh::Start))
                    .await;

                let retry_strategy = ExponentialBackoff::from_millis(50).map(jitter).take(10);

                let connection = Retry::spawn(retry_strategy, || async move {
                    AgentConnection::new(
                        config,
                        connect_info.clone(),
                        &mut NullReporter::default(),
                    )
                    .await
                    .inspect_err(
                        |err| tracing::error!(error = ?err, "unable to connect to agent upon retry"),
                    )
                })
                .await;

                match connection {
                    Ok(connection) => {
                        *self = connection;
                        message_bus
                            .send(ProxyMessage::ConnectionRefresh(ConnectionRefresh::End))
                            .await;

                        ControlFlow::Continue(())
                    }
                    Err(error) => {
                        tracing::error!(?error, "unable to reconnect agent");

                        ControlFlow::Break(AgentChannelError)
                    }
                }
            }
        }
    }
}
