use std::{net::IpAddr, sync::Arc};

use mirrord_protocol::{ClientMessage, DaemonMessage, AGENT_TLS_ENV};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
pub use tokio_rustls::rustls;
use tokio_rustls::{rustls::ServerConfig, TlsAcceptor};

use crate::{api::wrap_raw_connection, error::Result};

/// Used for creating a connection with a running agent.
/// Same variant must be used for creating the agent and connecting with it agent (see
/// [`Self::agent_extra_env`] method).
#[derive(Clone)]
pub enum AgentInclusterConnector {
    /// Uses a raw TCP connection.
    Tcp,
    /// Uses an encrypted TLS connection with the provided [`ServerConfig`].
    ///
    /// # Note
    ///
    /// The underlying TPC connection is initiated by us, but we are in fact the TLS server.
    Tls(Arc<ServerConfig>),
}

impl AgentInclusterConnector {
    /// Connects to the agent based on hostname and port. DNS resolution is performed.
    pub async fn connect_hostname(
        &self,
        hostname: &str,
        port: u16,
    ) -> Result<(Sender<ClientMessage>, Receiver<DaemonMessage>)> {
        let stream = TcpStream::connect((hostname, port)).await?;

        let wrapped = match self {
            Self::Tcp => wrap_raw_connection(stream),
            Self::Tls(config) => {
                let tls_stream = TlsAcceptor::from(config.clone()).accept(stream).await?;
                wrap_raw_connection(tls_stream)
            }
        };

        Ok(wrapped)
    }

    /// Connects to the agent based on IP address and port.
    pub async fn connect_ip(
        &self,
        ip: IpAddr,
        port: u16,
    ) -> Result<(Sender<ClientMessage>, Receiver<DaemonMessage>)> {
        let stream = TcpStream::connect((ip, port)).await?;

        let wrapped = match self {
            Self::Tcp => wrap_raw_connection(stream),
            Self::Tls(config) => {
                let tls_stream = TlsAcceptor::from(config.clone()).accept(stream).await?;
                wrap_raw_connection(tls_stream)
            }
        };

        Ok(wrapped)
    }

    /// Returns additional environment variables to be set in the agent container.
    pub fn agent_extra_env(&self) -> Vec<(String, String)> {
        match self {
            Self::Tcp => Default::default(),
            Self::Tls(..) => vec![(AGENT_TLS_ENV.to_string(), "true".to_string())],
        }
    }
}
