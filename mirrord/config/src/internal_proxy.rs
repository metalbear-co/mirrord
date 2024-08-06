use std::{net::SocketAddr, path::PathBuf};

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::source::MirrordConfigSource;

pub static MIRRORD_INTPROXY_CONNECT_TCP_ENV: &str = "MIRRORD_INTPROXY_CONNECT_TCP";
pub static MIRRORD_INTPROXY_DETACH_IO_ENV: &str = "MIRRORD_INTPROXY_DETACH_IO";

/// Configuration for the internal proxy mirrord spawns for each local mirrord session
/// that local layers use to connect to the remote agent
///
/// This is seldom used, but if you get `ConnectionRefused` errors, you might
/// want to increase the timeouts a bit.
///
/// ```json
/// {
///   "internal_proxy": {
///     "start_idle_timeout": 30,
///     "idle_timeout": 5
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize)]
#[config(map_to = "InternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct InternalProxyConfig {
    /// ### internal_proxy.connect_tcp {#internal_proxy-connect_tcp}
    ///
    ///
    ///
    /// ```json
    /// {
    ///   "internal_proxy": {
    ///     "connect_tcp": "10.10.0.100:7777"
    ///   }
    /// }
    /// ```
    #[config(env = MIRRORD_INTPROXY_CONNECT_TCP_ENV)]
    pub connect_tcp: Option<SocketAddr>,

    /// ### internal_proxy.client_tls_certificate {#internal_proxy-client_tls_certificate}
    ///
    /// Certificate to use as tls client credentials for connection to `connect_tcp`.
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = "MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE")]
    pub client_tls_certificate: Option<PathBuf>,

    /// ### internal_proxy.client_tls_key {#internal_proxy-client_tls_key}
    ///
    /// Private Key to use as tls client credentials for connection to `connect_tcp`.
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = "MIRRORD_INTPROXY_CLIENT_TLS_KEY")]
    pub client_tls_key: Option<PathBuf>,

    /// ### internal_proxy.start_idle_timeout {#internal_proxy-start_idle_timeout}
    ///
    /// How much time to wait for the first connection to the proxy in seconds.
    ///
    /// Common cases would be running with dlv or any other debugger, which sets a breakpoint
    /// on process execution, delaying the layer startup and connection to proxy.
    ///
    /// ```json
    /// {
    ///   "internal_proxy": {
    ///     "start_idle_timeout": 60
    ///   }
    /// }
    /// ```
    #[config(default = 60)]
    pub start_idle_timeout: u64,

    /// ### internal_proxy.idle_timeout {#internal_proxy-idle_timeout}
    ///
    /// How much time to wait while we don't have any active connections before exiting.
    ///
    /// Common cases would be running a chain of processes that skip using the layer
    /// and don't connect to the proxy.
    ///
    /// ```json
    /// {
    ///   "internal_proxy": {
    ///     "idle_timeout": 30
    ///   }
    /// }
    /// ```
    #[config(default = 5)]
    pub idle_timeout: u64,

    /// ### internal_proxy.log_level {#internal_proxy-log_level}
    /// Set the log level for the internal proxy.
    /// RUST_LOG convention (i.e `mirrord=trace`)
    /// will only be used if log_destination is set
    pub log_level: Option<String>,

    /// ### internal_proxy.log_destination {#internal_proxy-log_destination}
    /// Set the log file destination for the internal proxy.
    pub log_destination: Option<String>,

    /// ### internal_proxy.detach_io {#internal_proxy-detach_io}
    ///
    /// This makes the process not receive signals from the `mirrord` process or its parent
    /// terminal, preventing unwanted side effects.
    #[config(default = true, env = MIRRORD_INTPROXY_DETACH_IO_ENV)]
    pub detach_io: bool,
}
