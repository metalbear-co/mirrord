use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::config::source::MirrordConfigSource;

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
///     "idle_timeout": 5,
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "InternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct InternalProxyConfig {
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
    /// will only beu sed if log_destination is set
    pub log_level: Option<String>,

    /// ### internal_proxy.log_destination {#internal_proxy-log_destination}
    /// Set the log file destination for the internal proxy.
    pub log_destination: Option<String>,
}
