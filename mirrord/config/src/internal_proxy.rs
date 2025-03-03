use std::{net::SocketAddr, path::PathBuf};

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

pub static MIRRORD_INTPROXY_CONNECT_TCP_ENV: &str = "MIRRORD_INTPROXY_CONNECT_TCP";
pub static MIRRORD_INTPROXY_CONTAINER_MODE_ENV: &str = "MIRRORD_INTPROXY_CONTAINER_MODE";
pub static MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE_ENV: &str =
    "MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE";
pub static MIRRORD_INTPROXY_CLIENT_TLS_KEY_ENV: &str = "MIRRORD_INTPROXY_CLIENT_TLS_KEY";

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
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "InternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct InternalProxyConfig {
    /// <!--${internal}-->
    ///
    /// Address of external proxy to be used in `mirrord container`
    #[config(env = MIRRORD_INTPROXY_CONNECT_TCP_ENV)]
    pub connect_tcp: Option<SocketAddr>,

    /// <!--${internal}-->
    ///
    /// Certificate to use as tls client credentials for connection to `connect_tcp`.
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE_ENV)]
    pub client_tls_certificate: Option<PathBuf>,

    /// <!--${internal}-->
    ///
    /// Private Key to use as tls client credentials for connection to `connect_tcp`.
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = MIRRORD_INTPROXY_CLIENT_TLS_KEY_ENV)]
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

    /// <!--${internal}-->
    ///
    /// Sometimes the cpu is too busy with other tasks and the internal proxy sockets end
    /// up timing out. It's set at a ridiculous high value to prevent this from happening
    /// when a user hits a breakpoint while debugging, and stays stopped for a while, which
    /// sometimes results in mirrord not working when they resume.
    ///
    /// ```json
    /// {
    ///   "internal_proxy": {
    ///     "socket_timeout": 31536000
    ///   }
    /// }
    /// ```
    #[config(default = 31536000)]
    pub socket_timeout: u64,

    /// ### internal_proxy.log_level {#internal_proxy-log_level}
    ///
    /// Set the log level for the internal proxy.
    /// RUST_LOG convention (i.e `mirrord=trace`) will only be used if `log_destination`
    /// is set.
    pub log_level: Option<String>,

    /// ### internal_proxy.log_destination {#internal_proxy-log_destination}
    ///
    /// Set the log file destination for the internal proxy.
    #[config(default = crate::default_proxy_logfile_path("mirrord-intproxy"))]
    pub log_destination: PathBuf,

    /// <!--${internal}-->
    ///
    /// This informs the intproxy that it's running inside a continer and should not detach io
    #[config(default = false, env = MIRRORD_INTPROXY_CONTAINER_MODE_ENV)]
    pub container_mode: bool,

    /// ### internal_proxy.json_log {#internal_proxy-json_log}
    ///
    /// Whether the proxy should output logs in JSON format. If false, logs are output in
    /// human-readable format.
    #[config(default = true)]
    pub json_log: bool,
}
