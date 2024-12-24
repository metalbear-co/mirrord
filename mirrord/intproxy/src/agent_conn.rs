//! Implementation of `proxy <-> agent` connection through [`mpsc`] channels created in different
//! mirrord crates.

use std::{
    fs::File,
    io,
    io::BufReader,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

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
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::{
        mpsc,
        mpsc::{Receiver, Sender},
    },
};
use tokio_rustls::TlsConnector;
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    ProxyMessage,
};

/// Errors that can occur when the internal proxy tries to establish a connection with the agent.
#[derive(Error, Debug)]
pub enum AgentConnectionError {
    /// IO failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// mirrord operator API failed.
    #[error("{0}")]
    Operator(#[from] OperatorApiError),
    /// mirrord kube API failed.
    #[error("{0}")]
    Kube(#[from] KubeApiError),

    #[error("{0}")]
    Tls(#[from] ConnectionTlsError),

    /// The proxy failed to find a connection method in the provided [LayerConfig].
    #[error("invalid configuration, could not find method for connection")]
    NoConnectionMethod,
}

#[derive(Error, Debug)]
pub enum ConnectionTlsError {
    #[error("could not open pem data from {0}, error: {1}")]
    MissingPem(PathBuf, io::Error),

    #[error("could not parse pem data from {0}, error: {1}")]
    ParsingPem(PathBuf, io::Error),

    #[error("could not find a private_key after successfuly parsing {0}")]
    MissingPrivateKey(PathBuf),

    #[error("could not setup rustls::ClientConfig with provided certificate values: {0}")]
    ClientConfig(rustls::Error),

    #[error("could not setup rustls::WebPkiClientVerifier with provided certificate values: {0}")]
    ClientVerifier(rustls::client::VerifierBuilderError),

    #[error("could not setup rustls::ServerConfig with provided certificate values: {0}")]
    ServerConfig(rustls::Error),

    #[error("got invalid proxy addr for tls connection: {0}")]
    InvalidDnsName(IpAddr, rustls::pki_types::InvalidDnsNameError),

    #[error("could not create connection with tls: {0}")]
    Connection(io::Error),
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
                        proxy_addr.ip(),
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

pub async fn wrap_connection_with_tls(
    stream: TcpStream,
    proxy_addr: IpAddr,
    tls_certificate: &Path,
    client_tls_certificate: &Path,
    client_tls_key: &Path,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), ConnectionTlsError> {
    let mut root_cert_store = rustls::RootCertStore::empty();

    root_cert_store.add_parsable_certificates(
        rustls_pemfile::certs(&mut BufReader::new(File::open(tls_certificate).map_err(
            |error| ConnectionTlsError::MissingPem(tls_certificate.to_path_buf(), error),
        )?))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ConnectionTlsError::ParsingPem(tls_certificate.to_path_buf(), error))?,
    );

    let client_tls_certificate = rustls_pemfile::certs(&mut BufReader::new(
        File::open(client_tls_certificate).map_err(|error| {
            ConnectionTlsError::MissingPem(client_tls_certificate.to_path_buf(), error)
        })?,
    ))
    .collect::<Result<Vec<_>, _>>()
    .map_err(|error| ConnectionTlsError::ParsingPem(client_tls_certificate.to_path_buf(), error))?;

    let client_tls_keys = rustls_pemfile::private_key(&mut BufReader::new(
        File::open(client_tls_key)
            .map_err(|error| ConnectionTlsError::MissingPem(client_tls_key.to_path_buf(), error))?,
    ))
    .map_err(|error| ConnectionTlsError::ParsingPem(client_tls_key.to_path_buf(), error))?
    .ok_or_else(|| ConnectionTlsError::MissingPrivateKey(client_tls_key.to_path_buf()))?;

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_cert_store)
        .with_client_auth_cert(client_tls_certificate, client_tls_keys)
        .map_err(ConnectionTlsError::ClientConfig)?;

    let connector = TlsConnector::from(Arc::new(tls_config));

    let domain = rustls::pki_types::ServerName::try_from(proxy_addr.to_string())
        .map_err(|error| ConnectionTlsError::InvalidDnsName(proxy_addr, error))?
        .to_owned();

    Ok(wrap_raw_connection(
        connector
            .connect(domain, stream)
            .await
            .map_err(ConnectionTlsError::Connection)?,
    ))
}
