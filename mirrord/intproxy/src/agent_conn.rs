//! Implementation of `proxy <-> agent` connection through [`mpsc`](tokio::sync::mpsc) channels
//! created in different mirrord crates.

use std::{
    error::Report, fmt, io, net::SocketAddr, ops::ControlFlow, path::PathBuf, time::Duration,
};

use mirrord_analytics::{NullReporter, Reporter};
use mirrord_config::LayerConfig;
use mirrord_kube::{api::kubernetes::AgentKubernetesConnectInfo, error::KubeApiError, kube};
use mirrord_operator::{
    client::{
        OperatorApi, OperatorSession,
        error::{OperatorApiError, OperatorOperation},
    },
    types::{RECONNECT_NOT_POSSIBLE_CODE, RECONNECT_NOT_POSSIBLE_REASON},
};
#[cfg(test)]
use mirrord_protocol::DaemonMessage;
#[cfg(test)]
use mirrord_protocol_io::ConnectionOutput;
use mirrord_protocol_io::{Client, Connection, ProtocolError};
#[cfg(not(test))]
use serde::Deserialize;
use serde::Serialize;
use strum::IntoDiscriminant;
use strum_macros::EnumDiscriminants;
use thiserror::Error;
pub use tls::ConnectionTlsError;
use tokio::net::{TcpSocket, TcpStream};
#[cfg(test)]
use tokio::sync::mpsc;
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
    /// An error happened while communicating with the agent
    #[error("protocol error: {0}")]
    ProtocolError(#[from] ProtocolError),
}

/// Directive for the proxy on how to connect to the agent.
#[derive(Debug, Clone, Serialize, EnumDiscriminants)]
#[cfg_attr(not(test), derive(Deserialize))]
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
    /// Use a dummy connection. The sender is used for
    /// sending the new dummy connection to the driver code.
    ///
    /// For tests only.
    #[cfg(test)]
    Dummy(#[serde(skip)] mpsc::Sender<(mpsc::Sender<DaemonMessage>, ConnectionOutput<Client>)>),
}

impl fmt::Display for AgentConnectInfoDiscriminants {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::ExternalProxy => "external proxy",
            Self::Operator => "operator",
            Self::DirectKubernetes => "agent",
            #[cfg(test)]
            Self::Dummy => "dummy",
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
    RequestReconnect,
}

/// Handles logic of the `proxy <-> agent` connection as a [`BackgroundTask`].
///
/// # Note
/// The raw IO is managed in a separate [`tokio::task`] created in mirrord_protocol_io.
/// This differs from the [`LayerConnection`](crate::layer_conn::LayerConnection) implementation,
/// but this logic was already implemented elsewhere. This struct simply wraps the
/// [`mpsc`](tokio::sync::mpsc) channels returned from other functions and implements the
/// [`BackgroundTask`] trait.
pub struct AgentConnection {
    pub connection: Connection<Client>,
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

        let (connection, reconnect) = match connect_info {
            AgentConnectInfo::Operator(session) => {
                let connection =
                    OperatorApi::connect_in_existing_session(config, session.clone(), analytics)
                        .await?;
                (
                    connection.conn,
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

                let conn = match tls_pem {
                    Some(tls_pem) => tls::wrap_raw_connection(stream, tls_pem.as_path()).await?,
                    None => Connection::from_stream(stream).await?,
                };

                (conn, ReconnectFlow::Break(kind))
            }

            AgentConnectInfo::DirectKubernetes(connect_info) => {
                let conn = portforward::create_connection(config, connect_info.clone()).await?;
                (conn, ReconnectFlow::Break(kind))
            }

            #[cfg(test)]
            AgentConnectInfo::Dummy(sender) => {
                let (conn, tx, rx) = Connection::dummy();
                sender.send((tx, rx)).await.unwrap();

                let reconnect = ReconnectFlow::ConnectInfo {
                    config: config.clone(),
                    connect_info: AgentConnectInfo::Dummy(sender),
                };

                (conn, reconnect)
            }
        };

        Ok(Self {
            connection,
            reconnect,
        })
    }

    pub async fn new_for_raw_address(address: SocketAddr) -> Result<Self, AgentConnectionError> {
        let stream = TcpStream::connect(address).await?;
        let connection = Connection::<Client>::from_stream(stream).await?;

        Ok(Self {
            connection,
            reconnect: ReconnectFlow::Break(AgentConnectInfoDiscriminants::DirectKubernetes),
        })
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
                },

                msg = self.connection.recv() => match msg {
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
                            .send(ProxyMessage::ConnectionRefresh(ConnectionRefresh::End(
                                self.connection.tx_handle(),
                            )))
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
