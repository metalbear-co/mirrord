//! Implementation of `proxy <-> agent` connection through [`mpsc`] channels created in different
//! mirrord crates.

use std::{io, net::SocketAddr, path::Path, sync::Arc};

use mirrord_analytics::Reporter;
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{
        kubernetes::{AgentKubernetesConnectInfo, KubernetesAPI},
        wrap_raw_connection,
    },
    error::KubeApiError,
};
use mirrord_operator::client::{error::OperatorApiError, OperatorApi, OperatorSession};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use mirrord_tls_util::{
    rustls::{self, ClientConfig, RootCertStore},
    tokio_rustls::TlsConnector,
    CertChain, CertWithServerName, TlsUtilError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::{
        mpsc,
        mpsc::{Receiver, Sender},
    },
};
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    ProxyMessage,
};

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
    #[error("failed to make a TLS connection with the external proxy: {0}")]
    ExtproxyTls(#[from] ExtproxyConnectionTlsError),
    /// The proxy failed to find a connection method in the provided [LayerConfig].
    #[error("invalid configuration, could not find method for connection")]
    NoConnectionMethod,
}

/// Errors that can occur when internal proxy makes a TLS connection to the external proxy.
#[derive(Error, Debug)]
pub enum ExtproxyConnectionTlsError {
    #[error("failed to read external proxy certificate: {0}")]
    ReadExtproxyCertError(#[source] TlsUtilError),
    #[error("failed to add external proxy certificate as a root: {0}")]
    ExtproxyCertAsRootError(#[source] rustls::Error),
    #[error("failed to read internal proxy certificate chain: {0}")]
    ReadIntproxyCertChainError(#[source] TlsUtilError),
    #[error("internal proxy certificate chain is invalid: {0}")]
    InvalidIntproxyCertChain(#[source] rustls::Error),
    #[error("connection failed: {0}")]
    ConnectionFailed(#[source] io::Error),
}

/// Directive for the proxy on how to connect to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentConnectInfo {
    /// Connect to agent through `mirrord extproxy`.
    ExternalProxy(SocketAddr),
    /// Connect to the agent through the operator.
    Operator(OperatorSession),
    /// Connect directly to the agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
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
}

impl AgentConnection {
    /// Creates a new agent connection based on the provided [`LayerConfig`] and optional
    /// [`AgentConnectInfo`].
    pub async fn new<R: Reporter>(
        config: &LayerConfig,
        connect_info: Option<AgentConnectInfo>,
        analytics: &mut R,
    ) -> Result<Self, AgentConnectionError> {
        let (agent_tx, agent_rx) = match connect_info {
            Some(AgentConnectInfo::Operator(session)) => {
                let connection =
                    OperatorApi::connect_in_existing_session(config, session, analytics).await?;
                (connection.tx, connection.rx)
            }

            Some(AgentConnectInfo::ExternalProxy(proxy_addr)) => {
                let socket = TcpSocket::new_v4()?;
                socket.set_keepalive(true)?;
                socket.set_nodelay(true)?;

                let stream = socket.connect(proxy_addr).await?;

                if config.external_proxy.tls_enable
                    && let (
                        Some(tls_certificate),
                        Some(client_tls_certificate),
                        Some(client_tls_key),
                    ) = (
                        config.external_proxy.tls_certificate.as_ref(),
                        config.internal_proxy.client_tls_certificate.as_ref(),
                        config.internal_proxy.client_tls_key.as_ref(),
                    )
                {
                    wrap_connection_with_tls(
                        stream,
                        tls_certificate,
                        client_tls_certificate,
                        client_tls_key,
                    )
                    .await?
                } else {
                    wrap_raw_connection(stream)
                }
            }

            Some(AgentConnectInfo::DirectKubernetes(connect_info)) => {
                let k8s_api = KubernetesAPI::create(config)
                    .await
                    .map_err(AgentConnectionError::Kube)?;

                let stream = k8s_api
                    .create_connection_portforward(connect_info.clone())
                    .await
                    .map_err(AgentConnectionError::Kube)?;

                wrap_raw_connection(stream)
            }

            None => {
                let address = config
                    .connect_tcp
                    .as_ref()
                    .ok_or(AgentConnectionError::NoConnectionMethod)?;
                let stream = TcpStream::connect(address).await?;
                wrap_raw_connection(stream)
            }
        };

        Ok(Self { agent_tx, agent_rx })
    }

    pub async fn new_for_raw_address(address: SocketAddr) -> Result<Self, AgentConnectionError> {
        let stream = TcpStream::connect(address).await?;
        let (agent_tx, agent_rx) = wrap_raw_connection(stream);

        Ok(Self { agent_tx, agent_rx })
    }

    #[tracing::instrument(level = Level::TRACE, name = "send_agent_message", skip(self), ret)]
    async fn send(&self, msg: ClientMessage) -> Result<(), AgentChannelError> {
        self.agent_tx.send(msg).await.map_err(|_| AgentChannelError)
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

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
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
                }
            }
        }
    }
}

/// Accepts an established [`TcpStream`] with the external proxy, uses it to make a TLS connection
/// and returns [`mpsc`] handles to a [`tokio::task`] that operates the stream.
pub async fn wrap_connection_with_tls(
    stream: TcpStream,
    extproxy_tls_certificate: &Path,
    intproxy_tls_certificate: &Path,
    intproxy_tls_key: &Path,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), ExtproxyConnectionTlsError>
{
    let extproxy_cert = CertWithServerName::read(extproxy_tls_certificate)
        .map_err(ExtproxyConnectionTlsError::ReadExtproxyCertError)?;
    let server_name = extproxy_cert.server_name().clone();

    let mut root_store = RootCertStore::empty();
    root_store
        .add(extproxy_cert.into())
        .map_err(ExtproxyConnectionTlsError::ExtproxyCertAsRootError)?;

    let intproxy_chain = CertChain::read(intproxy_tls_certificate, intproxy_tls_key)
        .map_err(ExtproxyConnectionTlsError::ReadIntproxyCertChainError)?;
    let (cert_chain, key_der) = intproxy_chain.into_chain_and_key();

    let client_config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_client_auth_cert(cert_chain, key_der)
        .map_err(ExtproxyConnectionTlsError::InvalidIntproxyCertChain)?;
    let connector = TlsConnector::from(Arc::new(client_config));

    let stream = connector
        .connect(server_name, stream)
        .await
        .map_err(ExtproxyConnectionTlsError::ConnectionFailed)?;

    Ok(wrap_raw_connection(stream))
}
