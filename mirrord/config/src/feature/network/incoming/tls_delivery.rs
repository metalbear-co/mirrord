use std::path::PathBuf;

use rustls::pki_types::ServerName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{ConfigContext, ConfigError};

/// Stolen TLS traffic can be delivered to the local application either as TLS or as plain TCP.
/// Note that stealing TLS traffic requires mirrord Operator support.
///
/// To have the stolen TLS traffic delivered with plain TCP, use:
///
/// ```json
/// {
///   "protocol": "tcp"
/// }
/// ```
///
/// To have the traffic delivered with TLS, use:
/// ```json
/// {
///   "protocol": "tls"
/// }
/// ```
///
/// By default, the local mirrord TLS client will trust any certificate presented by the local
/// application's TLS server. To override this behavior, you can either:
///
/// 1. Specify a list of paths to trust roots. These paths can lead either to PEM files or PEM file
///    directories. Each found certificate will be used as a trust anchor.
/// 2. Specify a path to the cartificate chain used by the server.
///
/// Example with trust roots:
/// ```json
/// {
///   "protocol": "tls",
///   "trust_roots": ["/path/to/cert.pem", "/path/to/cert/dir"]
/// }
/// ```
///
/// Example with certificate chain:
/// ```json
/// {
///   "protocol": "tls",
///   "server_cert": "/path/to/cert.pem"
/// }
/// ```
///
/// To make a TLS connection to the local application's server,
/// mirrord's TLS client needs a server name. You can supply it manually like this:
/// ```json
/// {
///   "protocol": "tls",
///   "server_name": "my.test.server.name"
/// }
/// ```
///
/// If you don't supply the server name:
///
/// 1. If `server_cert` is given, and the found end-entity certificate contains a valid server name,
///    this server name will be used;
/// 2. Otherwise, if the original client supplied an SNI extension, the server name from that
///    extension will be used;
/// 3. Otherwise, if the stolen request's URL contains a valid server name, that server name will be
///    used;
/// 4. Otherwise, `localhost` will be used.
///
/// If the local application's TLS server requires a client certificate (mutual TLS), give
/// mirrord's TLS client one to present:
/// ```json
/// {
///   "protocol": "tls",
///   "client_cert": "/path/to/client.cert.pem",
///   "client_key": "/path/to/client.key.pem"
/// }
/// ```
///
/// In preview sessions (`mirrord preview start`) the mirrord operator makes the TLS connection to
/// the preview pod, so `client_cert` and `client_key` are read locally, stored in the session's
/// Secret and presented by the operator. `server_name` is used the same way. The other settings
/// do not apply to previews: the operator always delivers over TLS and does not verify the
/// preview pod's certificate.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LocalTlsDelivery {
    /// ##### feature.network.incoming.tls_delivery.protocol {#feature-network-incoming-tls_delivery-protocol}
    ///
    /// Protocol to use when delivering the TLS traffic locally. Defaults to `tls`.
    #[serde(default)]
    pub protocol: TlsDeliveryProtocol,

    /// ##### feature.network.incoming.tls_delivery.trust_roots {#feature-network-incoming-tls_delivery-trust_roots}
    ///
    /// Paths to PEM files and directories with PEM files containing allowed root certificates.
    ///
    /// Directories are not traversed recursively.
    ///
    /// Each certificate found in the files is treated as an allowed root.
    /// The files can contain entries of other types, e.g private keys, which are ignored.
    pub trust_roots: Option<Vec<PathBuf>>,

    /// ##### feature.network.incoming.tls_delivery.server_name {#feature-network-incoming-tls_delivery-server_name}
    ///
    /// Server name to use when making a connection.
    ///
    /// Must be a valid DNS name or an IP address.
    pub server_name: Option<String>,

    //// ##### feature.network.incoming.tls_delivery.server_cert
    //// {#feature-network-incoming-tls_delivery-server_cert}
    ///
    /// Path to a PEM file containing the certificate chain used by the local application's
    /// TLS server.
    ///
    /// This file must contain at least one certificate.
    /// It can contain entries of other types, e.g private keys, which are ignored.
    pub server_cert: Option<PathBuf>,

    /// ##### feature.network.incoming.tls_delivery.client_cert {#feature-network-incoming-tls_delivery-client_cert}
    ///
    /// Path to a PEM file containing the certificate chain mirrord presents to the local
    /// application's TLS server, for applications that require a client certificate.
    ///
    /// This file must contain at least one certificate. Must be set together with `client_key`.
    pub client_cert: Option<PathBuf>,

    /// ##### feature.network.incoming.tls_delivery.client_key {#feature-network-incoming-tls_delivery-client_key}
    ///
    /// Path to a PEM file containing the private key of `client_cert`.
    ///
    /// This file must contain exactly one private key. Must be set together with `client_cert`.
    pub client_key: Option<PathBuf>,
}

impl LocalTlsDelivery {
    pub fn verify(&self, _: &mut ConfigContext) -> Result<(), ConfigError> {
        match self {
            Self {
                protocol: TlsDeliveryProtocol::Tcp,
                ..
            } => {
                // other settings are ignored
            }
            Self {
                trust_roots: Some(..),
                server_cert: Some(..),
                ..
            } => {
                return Err(ConfigError::Conflict(
                    ".feature.network.incoming.tls_delivery.trust_roots and \
                    .feature.network.incoming.tls_delivery.server_cert cannot be specified together"
                        .into(),
                ));
            }
            Self {
                trust_roots: Some(roots),
                ..
            } if roots.is_empty() => {
                return Err(ConfigError::InvalidValue {
                    name: ".feature.network.incoming.tls_delivery.trust_roots".into(),
                    provided: "[]".into(),
                    error: "cannot be an empty list".into(),
                });
            }
            // A certificate cannot be presented without its key, and a key alone is useless.
            Self {
                client_cert: Some(..),
                client_key: None,
                ..
            }
            | Self {
                client_cert: None,
                client_key: Some(..),
                ..
            } => {
                return Err(ConfigError::Conflict(
                    ".feature.network.incoming.tls_delivery.client_cert and \
                    .feature.network.incoming.tls_delivery.client_key must be set together"
                        .into(),
                ));
            }
            _ => {}
        }

        if let Some(server_name) = self.server_name.as_deref()
            && ServerName::try_from(server_name).is_err()
        {
            return Err(ConfigError::InvalidValue {
                name: ".feature.network.incoming.tls_delivery.server_name".into(),
                provided: server_name.into(),
                error: "must be a valid DNS name or an IP address".into(),
            });
        }

        Ok(())
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsDeliveryProtocol {
    /// TLS traffic will be delivered over TCP.
    Tcp,
    /// TLS traffic will be delivered over TLS.
    #[default]
    Tls,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// A client certificate is only usable with its private key, so `client_cert` without
    /// `client_key` (or the reverse) is rejected before mirrord starts, instead of failing the
    /// first stolen request with a TLS alert.
    #[rstest]
    #[case::cert_only(Some("/certs/client.pem"), None)]
    #[case::key_only(None, Some("/certs/client.key"))]
    fn verify_rejects_client_cert_without_key(
        #[case] client_cert: Option<&str>,
        #[case] client_key: Option<&str>,
    ) {
        let config = LocalTlsDelivery {
            client_cert: client_cert.map(PathBuf::from),
            client_key: client_key.map(PathBuf::from),
            ..Default::default()
        };

        let mut context = ConfigContext::default();
        let error = config
            .verify(&mut context)
            .expect_err("half of a client identity must be rejected");
        assert!(
            matches!(error, ConfigError::Conflict(ref message) if message.contains("client_key")),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn verify_accepts_client_cert_with_key() {
        let config = LocalTlsDelivery {
            client_cert: Some(PathBuf::from("/certs/client.pem")),
            client_key: Some(PathBuf::from("/certs/client.key")),
            ..Default::default()
        };

        let mut context = ConfigContext::default();
        config
            .verify(&mut context)
            .expect("a full client identity is valid");
    }

    /// With `protocol: tcp` no TLS connection is made, so the other settings are not checked.
    #[test]
    fn verify_ignores_client_auth_for_tcp_delivery() {
        let config = LocalTlsDelivery {
            protocol: TlsDeliveryProtocol::Tcp,
            client_cert: Some(PathBuf::from("/certs/client.pem")),
            ..Default::default()
        };

        let mut context = ConfigContext::default();
        config
            .verify(&mut context)
            .expect("tcp delivery ignores TLS settings");
    }
}
