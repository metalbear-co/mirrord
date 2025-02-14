use std::{io, net::IpAddr, sync::Arc};

use http::Uri;
use nix::sys::socket::sockopt::IpAddMembership;
use rustls::{pki_types::ServerName, ClientConfig, ServerConfig, ServerConnection};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{client, server, TlsAcceptor, TlsConnector};

/// Provides a [`TlsAcceptor`] and a [`PassThroughTlsConnector`] to allow for filtered stealing on
/// TLS connections.
#[derive(Clone, Debug)]
pub struct StealTlsHandler {
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

impl StealTlsHandler {
    /// Returns a [`TlsAcceptor`] that can be used on stolen TCP connections.
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(self.server_config.clone())
    }

    /// Returns [`PassThroughTlsConnector`] that can be used on TCP connections with the original
    /// destination server.
    pub fn connector(mut self, original_connection: &ServerConnection) -> PassThroughTlsConnector {
        let server_name = original_connection
            .server_name()
            .and_then(|name| ServerName::try_from(name).ok()?.to_owned().into());
        let client_alpn = original_connection
            .alpn_protocol()
            .into_iter()
            .map(Vec::from)
            .collect::<Vec<_>>();

        let client_config = match Arc::get_mut(&mut self.client_config) {
            Some(client_config) => {
                client_config.alpn_protocols = client_alpn;
                self.client_config
            }
            None => {
                let mut client_config = self.client_config.as_ref().clone();
                client_config.alpn_protocols = client_alpn;
                Arc::new(client_config)
            }
        };

        let connector = TlsConnector::from(client_config);

        PassThroughTlsConnector {
            connector,
            server_name,
        }
    }
}

/// Allows for making TLS connections to the original destination server,
/// taking into account TLS handshake made previously in the stolen connection.
///
/// This allows us to use the same ALPN protocol and SNI extension as the original connection
/// source.
pub struct PassThroughTlsConnector {
    connector: TlsConnector,
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
    pub async fn connect<IO>(
        &self,
        server_ip: IpAddr,
        request_uri: Option<&Uri>,
        stream: IO,
    ) -> io::Result<client::TlsStream<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let server_name = self
            .server_name
            .clone()
            .or_else(|| Self::server_name_from_uri(request_uri?)?.to_owned().into())
            .unwrap_or_else(|| ServerName::from(server_ip));

        self.connector.connect(server_name, stream).await
    }

    /// Attempts to extract a [`ServerName`] from the given request [`Uri`].
    ///
    /// Copied from [hyper-tls](https://github.com/hyperium/hyper-tls/blob/0265e166a8886f01253050516316a95900315b81/src/client.rs#L140).
    fn server_name_from_uri(uri: &'_ Uri) -> Option<ServerName<'_>> {
        let hostname = uri.host()?.trim_matches(|c| c == '[' || c == ']');

        ServerName::try_from(hostname).ok()
    }
}
