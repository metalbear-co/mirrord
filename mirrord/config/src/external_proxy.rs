use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::config::source::MirrordConfigSource;

/// Configuration for the external proxy mirrord spawns for mirrord container support, this proxy is
/// used to allow intproxy running in sidecar to connect to agent
///
/// This is seldom used, but if you get `ConnectionRefused` errors, you might
/// want to increase the timeouts a bit.
///
/// ```json
/// {
///   "external_proxy": {
///     "start_idle_timeout": 30,
///     "idle_timeout": 5
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "ExternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct ExternalProxyConfig {
    /// ```json
    /// {
    ///   "exyernal_proxy": {
    ///     "connect_tcp": "10.10.0.100:7777"
    ///   }
    /// }
    /// ```
    #[config(env = "MIRRORD_EXTERNAL_CONNECT_TCP")]
    pub connect_tcp: Option<String>,

    /// ### external_proxy.start_idle_timeout {#external_proxy-start_idle_timeout}
    ///
    /// How much time to wait for the first connection to the proxy in seconds.
    ///
    /// Common cases would be running with dlv or any other debugger, which sets a breakpoint
    /// on process execution, delaying the layer startup and connection to proxy.
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

    /// ### external_proxy.log_level {#external_proxy-log_level}
    /// Set the log level for the external proxy.
    /// RUST_LOG convention (i.e `mirrord=trace`)
    /// will only be used if log_destination is set
    pub log_level: Option<String>,

    /// ### external_proxy.log_destination {#external_proxy-log_destination}
    /// Set the log file destination for the external proxy.
    pub log_destination: Option<String>,
}
