use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use self::{env::EnvConfig, fs::FsConfig, network::NetworkConfig};
use crate::config::source::MirrordConfigSource;

pub mod env;
pub mod fs;
pub mod network;

/// ## feature {#feature}
///
/// Configuration for mirrord features.
///
/// For more information, check the [technical reference](https://mirrord.dev/docs/reference/)
/// of the feature.
///
/// ### Minimal `feature` config {#feature-minimal}
///
/// The [`fs`](#fs) and [`network`](#network) options have support for a shortened version.
///
/// ```json
/// {
///   "feature": {
///     "env": {
///       "include": "DATABASE_USER;PUBLIC_ENV",
///       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
///       "overrides": {
///         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
///         "LOCAL_BEAR": "panda"
///       }
///     },
///     "fs": "read",
///     "network": "mirror",
///     "capture_error_trace": false
///   }
/// }
/// ```
///
/// ### Advanced `feature` config {#feature-advanced}
///
/// ```json
/// {
///   "feature": {
///     "env": {
///       "include": "DATABASE_USER;PUBLIC_ENV",
///       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
///       "overrides": {
///         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
///         "LOCAL_BEAR": "panda"
///       }
///     },
///     "fs": {
///       "mode": "write",
///       "read_write": ".+\.json" ,
///       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
///       "local": [ ".+\.js", ".+\.mjs" ]
///     },
///     "network": {
///       "incoming": {
///         "mode": "steal",
///         "http_header_filter": {
///           "filter": "host: api\..+",
///           "ports": [80, 8080]
///         },
///         "port_mapping": [{ 7777: 8888 }],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000]
///       },
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "ignore_localhost": false,
///         "unix_streams": "bear.+"
///       },
///       "dns": false
///     },
///     "capture_error_trace": false
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "FeatureFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct FeatureConfig {
    /// ### feature.env {#feature-env}
    ///
    /// Controls the environment variables feature, see [`env`](#env).
    ///
    /// For more information, check the environment variables
    /// [technical reference](https://mirrord.dev/docs/reference/env/).
    #[config(nested, toggleable)]
    pub env: EnvConfig,

    /// ### feature.fs {#feature-fs}
    ///
    /// Controls the file operations feature, see [`fs`](#fs).
    ///
    /// For more information, check the file operations
    /// [technical reference](https://mirrord.dev/docs/reference/fileops/).
    #[config(nested, toggleable)]
    pub fs: FsConfig,

    /// ### feature.network {#feature-network}
    ///
    /// Controls the network feature, see [`network`](#network).
    ///
    /// For more information, check the network traffic
    /// [technical reference](https://mirrord.dev/docs/reference/traffic/).
    #[config(nested, toggleable)]
    pub network: NetworkConfig,

    /// ### feature.capture_error_trace {#feature-capture_error_trace}
    ///
    /// Controls the crash reporting feature.
    ///
    /// With this feature enabled, mirrord generates a nice crash report log.
    ///
    /// Defaults to `false`.
    #[config(env = "MIRRORD_CAPTURE_ERROR_TRACE", default = false)]
    pub capture_error_trace: bool,
}
