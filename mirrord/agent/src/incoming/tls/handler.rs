use std::{fmt, io, net::IpAddr, ops::Not, sync::Arc};

use http::Uri;
use mirrord_protocol::tcp::{
    IncomingTrafficTransportType, TLS_CLIENT_IDENTITY_VERSION, TlsClientIdentity,
};
use mirrord_tls_util::{CertIdentity, CertNames, UriExt};
use rustls::{ClientConfig, ServerConfig, ServerConnection, pki_types::ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

use crate::util::protocol_version::ClientProtocolVersion;

/// [`ClientConfig`]s presenting certificates with known identities.
pub type IdentityClientConfigs = Vec<(CertIdentity, Arc<ClientConfig>)>;

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
    /// Like [`Self::client_config`], but presenting certificates configured in
    /// [`AgentClientConfig::identities`](mirrord_agent_env::steal_tls::AgentClientConfig::identities).
    ///
    /// Used instead of [`Self::client_config`] when the identity of the original client matches.
    pub(super) identity_client_configs: IdentityClientConfigs,
    /// Whether the certificates of the original clients are verified against trust roots.
    ///
    /// The identity from an unverified certificate is never used, as anyone could claim it.
    pub(super) verifies_clients: bool,
    /// Configured name to verify the original destination against when the stolen connection
    /// carries no SNI.
    pub(super) server_name: Option<ServerName<'static>>,
}

impl StealTlsHandler {
    /// Returns a [`TlsAcceptor`] that can be used on stolen TCP connections.
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(self.server_config.clone())
    }

    /// Returns [`PassThroughTlsConnector`] that can be used on TCP connections with the original
    /// destination server.
    pub fn connector(&self, original_connection: &ServerConnection) -> PassThroughTlsConnector {
        let server_name = original_connection
            .server_name()
            .and_then(|name| ServerName::try_from(name).ok()?.to_owned().into())
            .or_else(|| self.server_name.clone());
        let client_alpn = original_connection
            .alpn_protocol()
            .into_iter()
            .map(Vec::from)
            .collect::<Vec<_>>();
        let client_names = original_connection
            .peer_certificates()
            .filter(|_| self.verifies_clients)
            .and_then(|certs| certs.first())
            .and_then(|cert| CertNames::from_der(cert));

        let identity_config = client_names
            .as_ref()
            .filter(|_| self.identity_client_configs.is_empty().not())
            .and_then(|names| CertIdentity::new(&names.subject, &names.subject_alternative_names))
            .and_then(|identity| {
                self.identity_client_configs
                    .iter()
                    .enumerate()
                    .find(|(_, (candidate, _))| *candidate == identity)
            });
        tracing::debug!(
            has_client_cert = client_names.is_some(),
            identity_index = ?identity_config.map(|(index, _)| index),
            "Selected the client certificate for the passthrough connection \
            (index in `agentAsClient.identities`, or `authentication` if none)",
        );
        let base_config = identity_config
            .map(|(_, (_, config))| config)
            .unwrap_or(&self.client_config);
        let mut client_config = base_config.as_ref().clone();
        client_config.alpn_protocols = client_alpn;

        PassThroughTlsConnector {
            client_config: Arc::new(client_config),
            server_name,
            client_names,
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
    /// From the SNI extension received in the stolen connection, or else configured in the steal
    /// config.
    server_name: Option<ServerName<'static>>,
    /// Names from the verified certificate presented in the stolen connection.
    client_names: Option<CertNames>,
}

impl PassThroughTlsConnector {
    /// Makes to make client TLS connection in the given stream.
    ///
    /// [`TlsConnector::connect`] requires a [`ServerName`].
    /// We try to get it from following sources (in order of preference):
    /// 1. SNI from the original connection source (if supplied)
    /// 2. Name configured in the steal config (if supplied)
    /// 3. Request URI (if have a request)
    /// 4. Original destination ip
    ///
    /// Returns the [`TlsStream`] boxed, as its size exceeds 1kb.
    pub async fn connect<IO>(
        &self,
        server_ip: IpAddr,
        request_uri: Option<&Uri>,
        stream: IO,
    ) -> io::Result<Box<TlsStream<IO>>>
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
                    server_name = ?self.server_name,
                    alpn_protocol = ?self.client_config.alpn_protocols.first().map(|proto| String::from_utf8_lossy(proto)),
                    %error,
                    "Failed to make a TLS connection to the original destination.",
                );
            })
            .map(TlsStream::Client)
            .map(Box::new)
    }

    #[cfg(test)]
    pub fn server_name(&self) -> Option<&ServerName<'static>> {
        self.server_name.as_ref()
    }

    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.client_config
            .alpn_protocols
            .first()
            .map(|proto| proto.as_slice())
    }

    /// Describes the stolen connection's TLS session for an agent client, taking into account
    /// the client's [`mirrord_protocol`] version.
    pub fn transport_type(
        &self,
        protocol_version: &ClientProtocolVersion,
    ) -> IncomingTrafficTransportType {
        let alpn_protocol = self.alpn_protocol().map(Vec::from);
        let server_name = self.server_name.as_ref().map(|s| s.to_str().into_owned());

        if protocol_version.matches(&TLS_CLIENT_IDENTITY_VERSION) {
            IncomingTrafficTransportType::TlsV2 {
                alpn_protocol,
                server_name,
                client_identity: self.client_names.clone().map(
                    |CertNames {
                         subject,
                         subject_alternative_names,
                     }| TlsClientIdentity {
                        subject,
                        subject_alternative_names,
                    },
                ),
            }
        } else {
            IncomingTrafficTransportType::Tls {
                alpn_protocol,
                server_name,
            }
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
            .field("client_names", &self.client_names)
            .finish()
    }
}
