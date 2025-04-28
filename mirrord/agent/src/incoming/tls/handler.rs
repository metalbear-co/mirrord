use std::{fmt, io, net::IpAddr, sync::Arc};

use http::Uri;
use mirrord_protocol::tcp::TrafficTransportType;
use mirrord_tls_util::UriExt;
use rustls::{pki_types::ServerName, ClientConfig, ServerConfig, ServerConnection};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{client, TlsAcceptor, TlsConnector};

/// Provides a [`TlsAcceptor`] and a [`PassThroughTlsConnector`] to allow for filtered stealing on
/// TLS connections.
#[derive(Clone, Debug)]
pub struct IncomingTlsHandler {
    /// Constructing [`TlsAcceptor`] from this is cheap.
    ///
    /// We keep the config here for nice [`Debug`](std::fmt::Debug) derive.
    pub(super) server_config: Arc<ServerConfig>,
    /// We need to keep the config, because we'll possibly be filling
    /// [`ClientConfig::alpn_protocols`] when making the connection.
    ///
    /// Also [`Debug`](std::fmt::Debug) derive is nicer.
    pub(super) client_config: Arc<ClientConfig>,
}

impl IncomingTlsHandler {
    /// Returns a [`TlsAcceptor`] that can be used on stolen TCP connections.
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(self.server_config.clone())
    }

    /// Returns [`PassThroughTlsConnector`] that can be used on TCP connections with the original
    /// destination server.
    pub fn connector(&self, original_connection: &ServerConnection) -> PassThroughTlsConnector {
        let server_name = original_connection
            .server_name()
            .and_then(|name| ServerName::try_from(name).ok()?.to_owned().into());
        let client_alpn = original_connection
            .alpn_protocol()
            .into_iter()
            .map(Vec::from)
            .collect::<Vec<_>>();

        let mut client_config = self.client_config.as_ref().clone();
        client_config.alpn_protocols = client_alpn;

        PassThroughTlsConnector {
            client_config: Arc::new(client_config),
            server_name,
        }
    }
}

/// Allows for making TLS connections to the original destination server,
/// taking into account TLS handshake made previously in the stolen connection.
///
/// This allows us to use the same ALPN protocol and SNI extension as the original connection
/// source.
#[derive(Clone)]
pub struct PassThroughTlsConnector {
    /// Constructing [`TlsConnector`] from this is cheap.
    ///
    /// We keep the config here for richer tracing in [`Self::connect`].
    client_config: Arc<ClientConfig>,
    /// From the SNI extension received in the stolen connection.
    server_name: Option<ServerName<'static>>,
}

impl PassThroughTlsConnector {
    /// Makes to make client TLS connection in the given stream.
    ///
    /// [`TlsConnector::connect`] requires a [`ServerName`].
    /// We try to get it from following sources (in order of preference):
    /// 1. SNI from the original connection source (if supplied)
    /// 2. Request URI (if have a request)
    /// 3. Original destination ip
    ///
    /// Returns the [`client::TlsStream`] boxed, as its size exceeds 1kb.
    pub async fn connect<IO>(
        &self,
        server_ip: IpAddr,
        request_uri: Option<&Uri>,
        stream: IO,
    ) -> io::Result<Box<client::TlsStream<IO>>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let server_name = self
            .server_name
            .clone()
            .or_else(|| request_uri?.get_server_name()?.to_owned().into())
            .unwrap_or_else(|| ServerName::from(server_ip));

        let connector = TlsConnector::from(self.client_config.clone());

        connector
            .connect(server_name, stream)
            .await
            .inspect_err(|error| {
                tracing::warn!(
                    %server_ip,
                    ?request_uri,
                    original_sni = ?self.server_name,
                    alpn_protocol = ?self.client_config.alpn_protocols.first().map(|proto| String::from_utf8_lossy(proto)),
                    %error,
                    "Failed to make a TLS connection to the original destination.",
                );
            })
            .map(Box::new)
    }

    pub fn server_name(&self) -> Option<&ServerName<'static>> {
        self.server_name.as_ref()
    }

    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.client_config
            .alpn_protocols
            .first()
            .map(|proto| proto.as_slice())
    }
}

impl From<PassThroughTlsConnector> for TrafficTransportType {
    fn from(connector: PassThroughTlsConnector) -> Self {
        Self::Tls {
            alpn_protocol: connector.alpn_protocol().map(From::from),
            server_name: connector.server_name.map(|name| name.to_str().into()),
        }
    }
}

impl fmt::Debug for PassThroughTlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PassThroughTlsConnector")
            .field(
                "alpn_protocol",
                &self
                    .client_config
                    .alpn_protocols
                    .first()
                    .map(|proto| String::from_utf8_lossy(proto)),
            )
            .field("server_name", &self.server_name)
            .finish()
    }
}
