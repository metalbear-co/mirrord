use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::source::MirrordConfigSource,
    logfile_path::{Intproxy, LogDestinationConfig},
};

/// Environment variable we use to notify the internal proxy that it runs in a sidecar container.
///
/// If the internal proxy runs as a sidecar container, this variable should be set to `true`.
///
/// This affects how the internal proxy reads the [`LayerConfig`](crate::LayerConfig)
/// and handles logs.
pub const MIRRORD_INTPROXY_CONTAINER_MODE_ENV: &str = "MIRRORD_INTPROXY_CONTAINER_MODE";

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
    ///
    /// The value should follow the RUST_LOG convention (i.e `mirrord=trace`).
    ///
    /// Defaults to `mirrord=info,warn`.
    #[config(default = "mirrord=info,warn")]
    pub log_level: String,

    /// ### internal_proxy.log_destination {#internal_proxy-log_destination}
    ///
    /// Set the log destination for the internal proxy.
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
    pub log_destination: LogDestinationConfig<Intproxy>,

    /// ### internal_proxy.json_log {#internal_proxy-json_log}
    ///
    /// Whether the proxy should output logs in JSON format. If false, logs are output in
    /// human-readable format.
    ///
    /// Defaults to true.
    #[config(default = true)]
    pub json_log: bool,

    /// ### internal_proxy.process_logging_interval {#internal_proxy-process_logging_interval}
    ///
    /// How often to log information about connected processes in seconds.
    ///
    /// This feature logs details about processes that are currently connected to the internal
    /// proxy, including their PID, process name, command line, and connection status.
    ///
    /// ```json
    /// {
    ///   "internal_proxy": {
    ///     "process_logging_interval": 60
    ///   }
    /// }
    /// ```
    #[config(default = 60)]
    pub process_logging_interval: u64,
}
