use std::path::PathBuf;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::source::MirrordConfigSource;

/// Configuration for the external proxy mirrord spawns when using the `mirrord container` command.
/// This proxy is used to allow the internal proxy running in sidecar to connect to the mirrord
/// agent.
///
/// If you get `ConnectionRefused` errors, increasing the timeouts a bit might solve the issue.
///
/// ```json
/// {
///   "external_proxy": {
///     "start_idle_timeout": 30,
///     "idle_timeout": 5
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize)]
#[config(map_to = "ExternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct ExternalProxyConfig {
    /// ### external_proxy.tls_certificate {#external_proxy-tls_certificate}
    ///
    /// Certificate path to be used for wrapping external proxy tcp listener with a tcp acceptor
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = "MIRRORD_EXTERNAL_TLS_CERTIFICATE")]
    pub tls_certificate: Option<PathBuf>,

    /// ### external_proxy.tls_key {#external_proxy-tls_key}
    ///
    /// Private Key path to be used for wrapping external proxy tcp listener with a tcp acceptor
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = "MIRRORD_EXTERNAL_TLS_KEY")]
    pub tls_key: Option<PathBuf>,

    /// ### external_proxy.start_idle_timeout {#external_proxy-start_idle_timeout}
    ///
    /// How much time to wait for the first connection to the external proxy in seconds.
    ///
    /// Common cases would be running with dlv or any other debugger, which sets a breakpoint
    /// on process execution, delaying the layer startup and connection to the external proxy.
    ///
    /// ```json
    /// {
    ///   "external_proxy": {
    ///     "start_idle_timeout": 60
    ///   }
    /// }
    /// ```
    #[config(default = 60)]
    pub start_idle_timeout: u64,

    /// ### external_proxy.idle_timeout {#external_proxy-idle_timeout}
    ///
    /// How much time to wait while we don't have any active connections before exiting.
    ///
    /// Common cases would be running a chain of processes that skip using the layer
    /// and don't connect to the proxy.
    ///
    /// ```json
    /// {
    ///   "external_proxy": {
    ///     "idle_timeout": 30
    ///   }
    /// }
    /// ```
    #[config(default = 5)]
    pub idle_timeout: u64,

    /// ### external_proxy.log_level {#external_proxy-log_level}
    /// Sets the log level for the external proxy.
    ///
    /// Follows the `RUST_LOG` convention (i.e `mirrord=trace`), and will only be used if
    /// `external_proxy.log_destination` is set
    pub log_level: Option<String>,

    /// ### external_proxy.log_destination {#external_proxy-log_destination}
    /// Set the log file destination for the external proxy.
    pub log_destination: Option<String>,
}
