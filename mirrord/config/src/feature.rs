use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use self::{env::EnvConfig, fs::FsConfig, network::NetworkConfig};
use crate::config::source::MirrordConfigSource;

pub mod env;
pub mod fs;
pub mod network;

#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "FeatureFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct FeatureConfig {
    /// ## feature.env {#feature-env}
    ///
    /// Allows the user to set or override the local process' environment variables with the ones
    /// from the remote pod.
    ///
    /// Which environment variables to load from the remote pod are controlled by setting either
    /// [`include`](#feature-env-include) or [`exclude`](#feature-env-exclude).
    ///
    /// See the environment variables [reference](https://mirrord.dev/docs/reference/env/) for more details.
    ///
    /// ```json
    /// {
    ///   "feature": {
    ///     "env": {
    ///       "include": "DATABASE_USER;PUBLIC_ENV",
    ///       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
    ///       "override": {
    ///         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
    ///         "LOCAL_BEAR": "panda"
    ///       }
    ///     }
    ///   }
    /// }
    /// ```
    #[config(nested, toggleable)]
    pub env: EnvConfig,

    /// ## feature.fs {#feature-fs}
    ///
    /// Allows the user to specify the default behavior for file operations:
    ///
    /// 1. `"read"` - Read from the remote file system (default)
    /// 2. `"write"` - Read/Write from the remote file system.
    /// 3. `"local"` - Read from the local file system.
    /// 5. `"disable"` - Disable file operations.
    ///
    /// Besides the default behavior, the user can specify behavior for specific regex patterns.
    /// Case insensitive.
    ///
    /// 1. `"read_write"` - List of patterns that should be read/write remotely.
    /// 2. `"read_only"` - List of patterns that should be read only remotely.
    /// 3. `"local"` - List of patterns that should be read locally.
    ///
    /// The logic for choosing the behavior is as follows:
    ///
    /// 1. Check if one of the patterns match the file path, do the corresponding action. There's
    /// no specified order if two lists match the same path, we will use the first one (and we
    /// do not guarantee what is first).
    ///
    /// **Warning**: Specifying the same path in two lists is unsupported and can lead to undefined
    /// behaviour.
    ///
    /// 2. Check our "special list" - we have an internal at compile time list
    /// for different behavior based on patterns    to provide better UX.
    ///
    /// 3. If none of the above match, use the default behavior (mode).
    ///
    /// For more information, check the file operations
    /// [technical reference](https://mirrord.dev/docs/reference/fileops/).
    ///
    /// ```json
    /// {
    ///   "feature": {
    ///     "fs": {
    ///       "mode": "write",
    ///       "read_write": ".+\.json" ,
    ///       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
    ///       "local": [ ".+\.js", ".+\.mjs" ]
    ///     }
    ///   }
    /// }
    /// ```
    #[config(nested, toggleable)]
    pub fs: FsConfig,

    /// ## feature.network {#feature-network}
    ///
    /// Controls mirrord network operations.
    ///
    /// See the network traffic [reference](https://mirrord.dev/docs/reference/traffic/)
    /// for more details.
    ///
    /// ```json
    /// {
    ///   "feature": {
    ///     "network": {
    ///       "incoming": {
    ///         "mode": "steal",
    ///         "http_header_filter": {
    ///           "filter": "host: api\..+",
    ///           "ports": [80, 8080]
    ///         },
    ///         "port_mapping": [[ 7777, 8888 ]],
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
    ///     }
    ///   }
    /// }
    /// ```
    #[config(nested, toggleable)]
    pub network: NetworkConfig,

    /// ## feature.capture_error_trace {#feature-capture_error_trace}
    ///
    /// Controls the crash reporting feature.
    ///
    /// With this feature enabled, mirrord generates a nice crash report log.
    ///
    /// Defaults to `false`.
    #[config(env = "MIRRORD_CAPTURE_ERROR_TRACE", default = false)]
    pub capture_error_trace: bool,
}
