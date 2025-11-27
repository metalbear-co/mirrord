//! Implementation of `proxy <-> agent` connection through [`mpsc`](tokio::sync::mpsc) channels
//! created in different mirrord crates.

use std::{
    error::Report, fmt, io, net::SocketAddr, ops::ControlFlow, path::PathBuf, time::Duration,
};

use mirrord_analytics::{NullReporter, Reporter};
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{kubernetes::AgentKubernetesConnectInfo, wrap_raw_connection},
    error::KubeApiError,
    kube,
};
use mirrord_operator::{
    client::{
        OperatorApi, OperatorSession,
        error::{OperatorApiError, OperatorOperation},
    },
    types::{RECONNECT_NOT_POSSIBLE_CODE, RECONNECT_NOT_POSSIBLE_REASON},
};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use serde::{Deserialize, Serialize};
use strum::IntoDiscriminant;
use strum_macros::EnumDiscriminants;
use thiserror::Error;
pub use tls::ConnectionTlsError;
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::mpsc::{Receiver, Sender},
};
use tokio_retry::{RetryIf, strategy::ExponentialBackoff};
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
#[derive(Debug, Clone, Serialize, Deserialize, EnumDiscriminants)]
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

impl fmt::Display for AgentConnectInfoDiscriminants {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::ExternalProxy => "external proxy",
            Self::Operator => "operator",
            Self::DirectKubernetes => "agent",
        };

        f.write_str(as_str)
    }
}

pub enum ReconnectFlow {
    ConnectInfo {
        config: LayerConfig,
        connect_info: AgentConnectInfo,
    },

    Break(AgentConnectInfoDiscriminants),
}

impl ReconnectFlow {
    fn kind(&self) -> AgentConnectInfoDiscriminants {
        match self {
            Self::ConnectInfo { connect_info, .. } => connect_info.discriminant(),
            Self::Break(kind) => *kind,
        }
    }
}

impl ReconnectFlow {
    const fn reconnectable(&self) -> bool {
        matches!(self, ReconnectFlow::ConnectInfo { .. })
    }
}

impl fmt::Debug for ReconnectFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectInfo { connect_info, .. } => {
                f.debug_tuple("ConnectInfo").field(connect_info).finish()
            }
            Self::Break(connect_info_kind) => {
                f.debug_tuple("Break").field(connect_info_kind).finish()
            }
        }
    }
}

#[derive(Debug)]
pub enum AgentConnectionMessage {
    ClientMessage(ClientMessage),
    RequestReconnect,
}

impl From<ClientMessage> for AgentConnectionMessage {
    fn from(message: ClientMessage) -> Self {
        AgentConnectionMessage::ClientMessage(message)
    }
}

/// Handles logic of the `proxy <-> agent` connection as a [`BackgroundTask`].
///
/// # Note
/// The raw IO is managed in a separate [`tokio::task`] created in a different mirrord crate.
/// This differs from the [`LayerConnection`](crate::layer_conn::LayerConnection) implementation,
/// but this logic was already implemented elsewhere. This struct simply wraps the
/// [`mpsc`](tokio::sync::mpsc) channels returned from other functions and implements the
/// [`BackgroundTask`] trait.
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
        let kind = connect_info.discriminant();

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
                        ReconnectFlow::Break(kind)
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

                (tx, rx, ReconnectFlow::Break(kind))
            }

            AgentConnectInfo::DirectKubernetes(connect_info) => {
                let (tx, rx) = portforward::create_connection(config, connect_info.clone()).await?;
                (tx, rx, ReconnectFlow::Break(kind))
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
            reconnect: ReconnectFlow::Break(AgentConnectInfoDiscriminants::DirectKubernetes),
        })
    }

    #[tracing::instrument(level = Level::TRACE, name = "send_message_to_agent", skip(self), ret, err(level = Level::TRACE))]
    async fn send(&self, msg: ClientMessage) -> Result<(), AgentConnectionTaskError> {
        self.agent_tx
            .send(msg)
            .await
            .map_err(|_| AgentConnectionTaskError::ChannelError(self.reconnect.kind()))
    }

    pub fn reconnectable(&self) -> bool {
        self.reconnect.reconnectable()
    }
}

impl fmt::Debug for AgentConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConnection")
            .field("reconnect", &self.reconnect)
            .finish()
    }
}

#[derive(Error, Debug)]
pub enum AgentConnectionTaskError {
    #[error("{0} connection was requested to reconnect")]
    RequestedReconnect(AgentConnectInfoDiscriminants),
    /// This error occurs when the [`AgentConnection`] fails to communicate with the inner
    /// [`tokio::task`], which handles raw IO. The original (e.g. some IO error) is not available.
    #[error("{0} unexpectedly closed connection")]
    ChannelError(AgentConnectInfoDiscriminants),
}

impl BackgroundTask for AgentConnection {
    type Error = AgentConnectionTaskError;
    type MessageIn = AgentConnectionMessage;
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
                    Some(AgentConnectionMessage::RequestReconnect) => {
                        break Err(AgentConnectionTaskError::RequestedReconnect(self.reconnect.kind()))
                    }
                    Some(AgentConnectionMessage::ClientMessage(msg)) => {
                        if let Err(error) = self.send(msg).await {
                            tracing::error!(%error, "failed to send message to the {}", self.reconnect.kind());
                            break Err(error);
                        }
                    }
                },

                msg = self.agent_rx.recv() => match msg {
                    None => {
                        tracing::error!("failed to receive message from the {}, inner task down", self.reconnect.kind());
                        break Err(AgentConnectionTaskError::ChannelError(self.reconnect.kind()));
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
            ReconnectFlow::Break(..) => ControlFlow::Break(error),
            ReconnectFlow::ConnectInfo {
                config,
                connect_info,
            } => {
                match &error {
                    AgentConnectionTaskError::RequestedReconnect(..) => {
                        tracing::warn!(
                            ?connect_info,
                            "AgentConnection was requested to perform reconnect, attempting to reconnect"
                        );
                    }
                    _ => {
                        tracing::warn!(%error, ?connect_info, "AgentConnection experienced an error, attempting to reconnect");
                    }
                }

                message_bus
                    .send(ProxyMessage::ConnectionRefresh(ConnectionRefresh::Start))
                    .await;

                // 1s, 2s, 4s, 8s, 8s, ...
                let retry_strategy = ExponentialBackoff::from_millis(2)
                    .factor(500)
                    .max_delay(Duration::from_secs(8))
                    .take(10);
                // Unless the operator responded with explicit 410 (meaning that the session is
                // permanently gone), we can still retry.
                let can_retry = |error: &AgentConnectionError| match error {
                    AgentConnectionError::Operator(OperatorApiError::KubeError {
                        error: kube::Error::Api(error),
                        operation: OperatorOperation::WebsocketConnection,
                    }) => {
                        error.code != RECONNECT_NOT_POSSIBLE_CODE
                            || error.reason != RECONNECT_NOT_POSSIBLE_REASON
                    }
                    _ => true,
                };

                let connection = RetryIf::spawn(
                    retry_strategy,
                    || async {
                        message_bus
                            .closed_token()
                            .run_until_cancelled(AgentConnection::new(
                                config,
                                connect_info.clone(),
                                &mut NullReporter::default(),
                            ))
                            .await
                            .transpose()
                            .inspect_err(|error| {
                                tracing::error!(
                                    error = %Report::new(error),
                                    "Failed to reconnect to the {}",
                                    connect_info.discriminant(),
                                );
                            })
                    },
                    can_retry,
                )
                .await;

                match connection {
                    Ok(Some(connection)) => {
                        *self = connection;
                        message_bus
                            .send(ProxyMessage::ConnectionRefresh(ConnectionRefresh::End))
                            .await;

                        ControlFlow::Continue(())
                    }
                    Ok(None) | Err(..) => ControlFlow::Break(
                        AgentConnectionTaskError::ChannelError(self.reconnect.kind()),
                    ),
                }
            }
        }
    }
}
