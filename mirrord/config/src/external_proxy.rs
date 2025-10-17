use std::net::IpAddr;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::source::MirrordConfigSource,
    logfile_path::{Extproxy, LogDestinationConfig},
};

/// Environment variable we use to pass the TLS PEM file path to the external proxy.
pub const MIRRORD_EXTPROXY_TLS_SETUP_PEM: &str = "MIRRORD_EXTPROXY_TLS_PEM";

/// [`ServerName`](rustls::pki_types::ServerName) for the external proxy server certificate.
pub const MIRRORD_EXTPROXY_TLS_SERVER_NAME: &str = "extproxy";

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
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "ExternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct ExternalProxyConfig {
    /// <!--${internal}-->
    ///
    /// Whether to use TLS or a plain TCP when accepting a connection from the internal proxy
    /// sidecar.
    #[config(default = true)]
    pub tls_enable: bool,

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
    ///
    /// Set the log level for the external proxy.
    ///
    /// The value should follow the RUST_LOG convention (i.e `mirrord=trace`).
    ///
    /// Defaults to `mirrord=info,warn`.
    #[config(default = "mirrord=info,warn")]
    pub log_level: String,

    /// ### external_proxy.log_destination {#external_proxy-log_destination}
    ///
    /// Set the log destination for the external proxy.
    ///
    /// If the provided path ends with a separator (`/` on UNIX, `\` on Windows),
    /// it will be treated as a path to directory where the log file should be created.
    /// Otherwise, if the path exists, mirrord will check if it's a directory or not.
    /// Otherwise, it will be treated as a path to the log file.
    ///
    /// mirrord will auto create all parent directories.
    ///
    /// Defaults to a randomized path inside the temporary directory.
    #[config(default, nested)]
    pub log_destination: LogDestinationConfig<Extproxy>,

    /// ### external_proxy.json_log {#external_proxy-json_log}
    ///
    /// Whether the proxy should output logs in JSON format. If false, logs are output in
    /// human-readable format.
    ///
    /// Defaults to true.
    #[config(default = true)]
    pub json_log: bool,

    /// ### external_proxy.host_ip {#external_proxy-host_ip}
    ///
    /// Specify a custom host ip addr to listen on.
    ///
    /// This address must be accessible from within the container.
    /// If not specified, mirrord will try and resolve a local address to use.
    ///
    /// - If you're running inside WSL, and encountering problems, try setting this to `0.0.0.0`,
    ///   and `container.override_host_ip` to the internal container runtime address (for docker,
    ///   this would be what `host.docker.internal` resolved to, which by default is
    ///   `192.168.65.254`).
    pub host_ip: Option<IpAddr>,
}
