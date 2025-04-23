//! This module contains definition of TLS steal/mirror configuration for the agent.
//!
//! These structs are also used in the CRDs fetched by the operator.
//!
//! As with all definitions in this crate, keep this backwards compatible.

use std::{ops::Not, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Configures how a TLS client or server should authenticate itself.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TlsAuthentication {
    /// Path to a PEM file containing a certificate chain to use.
    ///
    /// This file must contain at least one certificate.
    /// It can contain entries of other types, e.g private keys, which are ignored.
    pub cert_pem: PathBuf,
    /// Path to a PEM file containing a private key matching the certificate chain found in
    /// `cert_pem`.
    ///
    /// This file must contain exactly one private key.
    /// It can contain entries of other types, e.g certificates, which are ignored.
    pub key_pem: PathBuf,
}

/// Configures how a TLS client should be verified.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TlsClientVerification {
    /// Whether anonymous clients should be accepted.
    ///
    /// Optional. Defaults to `false`.
    #[serde(default, skip_serializing_if = "Not::not")]
    pub allow_anonymous: bool,
    /// Whether to accept any certificate, regardless of its validity and who signed it.
    ///
    /// Note that this setting does not affect whether anononymous clients are accepted or not.
    /// If `allow_anonymous` is not set, a certificate will still be required.
    ///
    /// Optional. Defaults to `false`.
    #[serde(default, skip_serializing_if = "Not::not")]
    pub accept_any_cert: bool,
    /// Paths to PEM files and directories with PEM files containing allowed root certificates.
    ///
    /// Directories are not traversed recursively.
    ///
    /// Each certificate found in the files is treated as an allowed root.
    /// The files can contain entries of other types, e.g private keys, which are ignored.
    ///
    /// Optional. Defaults to an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_roots: Vec<PathBuf>,
}

/// Configures how a TLS server should be verified.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TlsServerVerification {
    /// Whether to accept any certificate, regardless of its validity and who signed it.
    ///
    /// Optional. Defaults to `false`.
    #[serde(default, skip_serializing_if = "Not::not")]
    pub accept_any_cert: bool,
    /// Paths to PEM files and directories with PEM files containing allowed root certificates.
    ///
    /// Directories are not traversed recursively.
    ///
    /// Each certificate found in the files is treated as an allowed root.
    /// The files can contain entries of other types, e.g private keys, which are ignored.
    ///
    /// Optional. Defaults to an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_roots: Vec<PathBuf>,
}

/// Configures mirrord-agent's TLS server.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentServerConfig {
    /// Configures how the server authenticates itself to the clients.
    pub authentication: TlsAuthentication,
    /// ALPN protocols supported by the server, in order of preference.
    ///
    /// If empty, ALPN is disabled.
    ///
    /// Optional. Defaults to en ampty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alpn_protocols: Vec<String>,
    /// Configures how mirrord-agent's server verifies the clients.
    ///
    /// Optional. If not present, the server will not offer client authentication at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<TlsClientVerification>,
}

/// Configures how mirrord-agent authenticates itself and the server when making TLS connections to
/// the original destination (which is the TLS server running in the target container).
///
/// The agent makes TLS connections to the original destination when passing through unmatched HTTPS
/// requests.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentClientConfig {
    /// Configures how mirrord-agent authenticates itself to the original destination server.
    ///
    /// Optional. If not present, mirrord-agent will make connections anonymously.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<TlsAuthentication>,
    /// Configures how mirrord-agent verifies the server's certificate.
    pub verification: TlsServerVerification,
}

/// Configures TLS setup for stealing/mirroring traffic from some port.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct IncomingPortTlsConfig {
    /// Remote port to which this configuration applies.
    pub port: u16,
    /// Configures how mirrord-agent authenticates itself and the clients when acting as a TLS
    /// server.
    ///
    /// mirrord-agent acts as a TLS server when accepting redirected connections.
    pub agent_as_server: AgentServerConfig,
    /// Configures how mirrord-agent authenticates itself and the server when acting as a TLS
    /// client.
    ///
    /// mirrord-agent acts as a TLS client when passing unmatched requests to their original
    /// destination.
    pub agent_as_client: AgentClientConfig,
}
