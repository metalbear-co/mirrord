use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

pub type StealTlsConfig = HashMap<u16, StealPortTlsConfig>;

/// Configures how mirrord-agent authenticates itself and the clients when acting as a TLS server.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct AgentServerAuth {
    /// Path to a PEM file containing a certificate chain to use for authenticating the
    /// mirrord-agent.
    ///
    /// This file must contain at least one certificate.
    /// It can contain entries of other types, e.g private keys, which are ignored.
    pub cert_pem: PathBuf,
    /// Path to a PEM file containing a private key matching [`AgentServerAuth::cert_pem`].
    ///
    /// This file must contain exactly one private key.
    /// It can contain entries of other types, e.g certificates, which are ignored.
    pub key_pem: PathBuf,
    /// Supported ALPN protocols.
    ///
    /// If empty, ALPN is disabled.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub alpn_protocols: Vec<AlpnProtocol>,
    /// Configures how mirrord-agent authenticates the clients.
    ///
    /// If not present, mirrord-agent's TLS server will not offer client authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_auth: Option<RemoteClientAuth>,
}

/// Configures how mirrord-agent authenticates clients when accepting stolen TLS connections.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct RemoteClientAuth {
    /// Whether anonymous clients should be accepted.
    pub allow_anonymous: bool,
    /// Paths to PEM files and directories with PEM files containing allowed root certificates.
    ///
    /// Directories are not traversed recursively.
    ///
    /// Each certificate found in the files is treated as an allowed root.
    /// The files can contain entries of other types, e.g private keys, which are ignored.
    ///
    /// Invalid certificates and files are ignored. However, unless
    /// [`RemoteClientAuth::allow_anonymous`] is set, we require at least one good
    /// certificate.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub root_cert_pems: Vec<PathBuf>,
}

/// Configures how mirrord-agent authenticates itself and the server when making TLS connections to
/// the original destination (which is the TLS server running in the target container).
///
/// The agent makes TLS connections to the original destination when passing through unmatched HTTPS
/// requests.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct AgentClientAuth {
    /// Path to a PEM file containing a certificate chain to use for authenticating the
    /// mirrord-agent.
    ///
    /// This file must contain at least one certificate.
    /// It can contain entries of other types, e.g private keys, which are ignored.
    pub cert_pem: PathBuf,
    /// Path to a PEM file containing a private key matching [`AgentClientAuth::cert_pem`].
    ///
    /// This file must contain exactly one private key.
    /// It can contain entries of other types, e.g certificates, which are ignored.
    pub key_pem: PathBuf,
    /// Configures how mirrord-agent authenticates the server.
    ///
    /// If not present, mirrord-agent's TLS client will accept **any** certificate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_auth: Option<RemoteServerAuth>,
}

/// Configures how mirrord-agent authenticates the server when making TLS connections to the
/// original destination (which is the TLS server running in the target container).
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct RemoteServerAuth {
    /// Paths to PEM files and directories with PEM files containing allowed root certificates.
    ///
    /// Directories are not traversed recursively.
    ///
    /// Each certificate found in the files is treated as an allowed root.
    /// The files can contain entries of other types, e.g private keys, which are ignored.
    ///
    /// However, we require at least one good certificate.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub root_cert_pems: Vec<PathBuf>,
}

/// Configures TLS setup for HTTPS stealing on some port.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct StealPortTlsConfig {
    /// Configures how mirrord-agent authenticates itself and the clients when acting as a TLS
    /// server.
    ///
    /// mirrord-agent acts as a TLS server when handling stolen connections.
    pub agent_server_auth: AgentServerAuth,
    /// Configures how mirrord-agent authenticates itself and the server when acting as a TLS
    /// client.
    ///
    /// mirrord-agent acts as a TLS client when passing unmatched requests to their original
    /// destinations.
    pub agent_client_auth: AgentClientAuth,
}

/// Protocol supported in [ALPN](https://www.rfc-editor.org/rfc/rfc7301.html).
///
/// See [complete list](https://www.iana.org/assignments/tls-extensiontype-values/tls-extensiontype-values.xhtml#alpn-protocol-ids).
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum AlpnProtocol {
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "http/1.0")]
    Http10,
    #[serde(rename = "http/1.1")]
    Http11,
    /// For future compatibility.
    #[serde(other)]
    Other,
}

impl AlpnProtocol {
    /// Returns name of this protocol as bytes,
    /// in the format expected by the `rustls` crate.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        let bytes: &[u8] = match self {
            Self::H2 => b"h2",
            Self::Http10 => b"http/1.0",
            Self::Http11 => b"http/1.1",
            Self::Other => return None,
        };

        Some(bytes)
    }
}
