use std::{
    fmt, io,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use http::Uri;
use mirrord_tls_util::{
    rustls::{pki_types::ServerName, ClientConfig, ServerConnection},
    tokio_rustls::{client, server, TlsAcceptor, TlsConnector},
    UriExt,
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Clone)]
pub struct StealTlsHandler {
    built_at: Instant,
    acceptor: TlsAcceptor,
    client_config: ClientConfig,
}

impl StealTlsHandler {
    pub fn new(acceptor: TlsAcceptor, client_config: ClientConfig) -> Self {
        Self {
            built_at: Instant::now(),
            acceptor,
            client_config,
        }
    }

    pub fn age(&self) -> Duration {
        self.built_at.elapsed()
    }

    pub async fn accept<IO>(&self, stream: IO) -> io::Result<server::TlsStream<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.acceptor.accept(stream).await
    }

    pub fn into_connector(
        mut self,
        original_connection: &ServerConnection,
    ) -> PassthroughTlsConnector {
        let server_name = original_connection
            .server_name()
            .and_then(|name| ServerName::try_from(name).ok()?.to_owned().into());

        let client_alpn = original_connection
            .alpn_protocol()
            .map(Vec::from)
            .map(|proto| vec![proto])
            .unwrap_or_default();
        self.client_config.alpn_protocols = client_alpn;

        PassthroughTlsConnector {
            client_config: self.client_config.into(),
            server_name,
        }
    }
}

impl fmt::Debug for StealTlsHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsHandler")
            .field("age_ms", &self.age().as_millis())
            .finish()
    }
}

#[derive(Clone)]
pub struct PassthroughTlsConnector {
    client_config: Arc<ClientConfig>,
    server_name: Option<ServerName<'static>>,
}

impl PassthroughTlsConnector {
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
            .or_else(|| request_uri?.server_name().ok()?.to_owned().into())
            .unwrap_or_else(|| ServerName::from(server_ip));
        let connector = TlsConnector::from(self.client_config.clone());
        connector.connect(server_name, stream).await
    }

    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        Some(self.client_config.alpn_protocols.first()?.as_slice())
    }

    pub fn server_name(&self) -> Option<&ServerName<'static>> {
        self.server_name.as_ref()
    }
}

impl fmt::Debug for PassthroughTlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let alpn_protocol = self.alpn_protocol().map(String::from_utf8_lossy);

        f.debug_struct("PassthroughTlsConnector")
            .field("alpn_protocol", &alpn_protocol)
            .field("server_name", &self.server_name)
            .finish()
    }
}
