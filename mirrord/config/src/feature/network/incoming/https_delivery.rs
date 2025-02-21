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
/// By default, the local mirrord TLS client will trust any server certificate.
/// To override this behavior, you can specify a list of paths to trust roots.
/// These paths can lead either to PEM files or PEM file directories.
/// Each found certificate will be used as a trust anchor.
///
/// ```json
/// {
///   "protocol": "tls",
///   "trust_roots": ["/path/to/cert.pem", "/path/to/cert/dir"]
/// }
/// ```
///
/// By default, the local mirrord TLS client will connect to the local server using the server name
/// from the original client's SNI extension. When such SNI is not supplied,
/// mirrord will extract the host name from the request URL.
/// When this fails, mirrord will use the local server's IP address.
///
/// You can override this behavior by supplying a server name to use.
///
/// ```json
/// {
///   "protocol": "tls",
///   "server_name": "127.0.0.1:9999"
/// }
/// ```
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
    ///
    /// Optional. If not given, mirrord will not verify the local server's certificate at all.
    pub trust_roots: Option<Vec<PathBuf>>,
    /// ##### feature.network.incoming.https_delivery.server_name {#feature-network-incoming-https_delivery-server_name}
    ///
    /// Server name to use when making a connection.
    ///
    /// Must be a valid DNS name or an IP address.
    ///
    /// Optional. If not given, mirrord will try to use
    /// the server name supplied in the SNI extension
    /// from the original client. When such SNI is not supplied,
    /// mirrord will extract the host name from the request URL.
    /// When this fails, mirrord will use the local server's IP address.
    pub server_name: Option<String>,
}

impl LocalHttpsDelivery {
    pub fn verify(&self, context: &mut ConfigContext) -> Result<(), ConfigError> {
        if self.protocol == HttpsDeliveryProtocol::Tcp {
            if self.trust_roots.is_some() || self.server_name.is_some() {
                context.add_warning("warning".into());
            }

            return Ok(());
        }

        if self
            .trust_roots
            .as_ref()
            .is_some_and(|roots| roots.is_empty())
        {
            return Err(ConfigError::InvalidValue {
                name: ".feature.network.incoming.https_delivery.trust_roots",
                provided: "[]".into(),
                error: "cannot be an empty list".into(),
            });
        }

        if let Some(server_name) = self.server_name.as_deref() {
            if ServerName::try_from(server_name).is_err() {
                return Err(ConfigError::InvalidValue {
                    name: ".feature.network.incoming.https_delivery.server_name",
                    provided: server_name.into(),
                    error: "must be a valif DNS name or an IP address".into(),
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
