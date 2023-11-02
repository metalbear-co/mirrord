use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use self::{env::EnvConfig, fs::FsConfig, network::NetworkConfig};
use crate::MirrordConfigSource;

pub mod env;
pub mod fs;
pub mod network;

/// Controls mirrord features.
///
/// See the
/// [technical reference, Technical Reference](https://mirrord.dev/docs/reference/)
/// to learn more about what each feature does.
///
/// The [`env`](#feature-env), [`fs`](#feature-fs) and [`network`](#feature-network) options
/// have support for a shortened version, that you can see [here](#root-shortened).
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
///         "port_mapping": [[ 7777, 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000]
///       },
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "filter": {
///           "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
///         },
///         "ignore_localhost": false,
///         "unix_streams": "bear.+"
///       },
///       "dns": false
///     },
///     "copy_target": false
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "FeatureFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct FeatureConfig {
    /// ## feature.env {#feature-env}
    #[config(nested, toggleable)]
    pub env: EnvConfig,

    // TODO(alex) [high] 2023-05-18: This links to `FsConfig`, not `FsUserConfig` as I thought
    // before.
    /// ## feature.fs {#feature-fs}
    #[config(nested, toggleable)]
    pub fs: FsConfig,

    /// ## feature.network {#feature-network}
    #[config(nested, toggleable)]
    pub network: NetworkConfig,

    /// ## feature.copy_target {#feature-copy-target}
    ///
    /// Creates a new copy of the target. mirrord will use this copy instead of the original target
    /// (e.g. intercept network traffic). This feature requires a [mirrord operator](https://mirrord.dev/docs/teams/introduction/).
    #[config(default = false, unstable)]
    pub copy_target: bool,
}

impl CollectAnalytics for &FeatureConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("env", &self.env);
        analytics.add("fs", &self.fs);
        analytics.add("network", &self.network);
        analytics.add("copy_target", self.copy_target);
    }
}
