use std::path::PathBuf;

use rustls::pki_types::ServerName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{ConfigContext, ConfigError};

/// Stolen HTTPS requests can be delivered to the local application either as HTTPS or as plain HTTP
/// requests. Note that stealing HTTPS requests requires mirrord Operator support.
///
/// To have the stolen HTTPS requests delivered with plain HTTP, use:
///
/// ```json
/// {
///   "protocol": "tcp"
/// }
/// ```
///
/// To have the requests delivered with HTTPS, use:
/// ```json
/// {
///   "protocol": "tls"
/// }
/// ```
///
/// By default, the local mirrord TLS client will trust any certificate presented by the local
/// application's HTTP server. To override this behavior, you can either:
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
/// To make a TLS connection to the local application's HTTPS server,
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
/// 3. Otherwise, host name from the stolen request's URL will be used;
/// 4. Otherwise, "localhost" will be used.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq, Default)]
pub struct LocalHttpsDelivery {
    /// ##### feature.network.incoming.https_delivery.protocol {#feature-network-incoming-https_delivery-protocol}
    ///
    /// Protocol to use when delivering the HTTPS requests locally.
    pub protocol: HttpsDeliveryProtocol,

    /// ##### feature.network.incoming.https_delivery.trust_roots {#feature-network-incoming-https_delivery-trust_roots}
    ///
    /// Paths to PEM files and directories with PEM files containing allowed root certificates.
    ///
    /// Directories are not traversed recursively.
    ///
    /// Each certificate found in the files is treated as an allowed root.
    /// The files can contain entries of other types, e.g private keys, which are ignored.
    pub trust_roots: Option<Vec<PathBuf>>,

    /// ##### feature.network.incoming.https_delivery.server_name {#feature-network-incoming-https_delivery-server_name}
    ///
    /// Server name to use when making a connection.
    ///
    /// Must be a valid DNS name or an IP address.
    pub server_name: Option<String>,

    //// ##### feature.network.incoming.https_delivery.server_cert
    //// {#feature-network-incoming-https_delivery-server_cert}
    ///
    /// Path to a PEM file containing the certificate chain used by the local application's HTTPS
    /// server.
    ///
    /// This file must contain at least one certificate.
    /// It can contain entries of other types, e.g private keys, which are ignored.
    pub server_cert: Option<PathBuf>,
}

impl LocalHttpsDelivery {
    pub fn verify(&self, _: &mut ConfigContext) -> Result<(), ConfigError> {
        match self {
            Self { protocol: HttpsDeliveryProtocol::Tcp, .. } => {
                // other settings are ignored
            },
            Self { trust_roots: Some(..), server_cert: Some(..), .. } => {
                return Err(ConfigError::Conflict(
                    ".feature.network.incoming.https_delivery.trust_roots and \
                    .feature.network.incoming.https_delivery.server_cert cannot be specified together".into()
                ))
            }
            Self { trust_roots: Some(roots), .. } if roots.is_empty() => {
                return Err(ConfigError::InvalidValue {
                    name: ".feature.network.incoming.https_delivery.trust_roots",
                    provided: "[]".into(),
                    error: "cannot be an empty list".into(),
                })
            }
            _ => {}
        }

        if let Some(server_name) = self.server_name.as_deref() {
            if ServerName::try_from(server_name).is_err() {
                return Err(ConfigError::InvalidValue {
                    name: ".feature.network.incoming.https_delivery.server_name",
                    provided: server_name.into(),
                    error: "must be a valid DNS name or an IP address".into(),
                });
            }
        }

        Ok(())
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HttpsDeliveryProtocol {
    /// HTTPS requests will be delivered over TCP, as plain HTTP.
    Tcp,
    /// HTTPS requests will be delivered over TLS, as HTTPS.
    #[default]
    Tls,
}
