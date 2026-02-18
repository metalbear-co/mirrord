use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use self::{
    copy_target::CopyTargetConfig, env::EnvConfig, fs::FsConfig, network::NetworkConfig,
    preview::PreviewConfig,
};
use crate::{
    config::source::MirrordConfigSource,
    feature::{database_branches::DatabaseBranchesConfig, split_queues::SplitQueuesConfig},
};

pub mod copy_target;
pub mod database_branches;
pub mod env;
pub mod fs;
pub mod network;
pub mod preview;
pub mod split_queues;

/// Controls mirrord features.
///
/// See the
/// [technical reference, Technical Reference](https://metalbear.com/mirrord/docs/reference/)
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
///       "read_write": ".+\\.json" ,
///       "read_only": [ ".+\\.yaml", ".+important-file\\.txt" ],
///       "local": [ ".+\\.js", ".+\\.mjs" ]
///     },
///     "network": {
///       "incoming": {
///         "mode": "steal",
///         "http_filter": {
///           "header_filter": "host: api\\..+"
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
///     "copy_target": false,
///     "hostname": true
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "FeatureFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct FeatureConfig {
    /// ### feature.env {#feature-env}
    #[config(nested, toggleable)]
    pub env: EnvConfig,

    // TODO(alex) [high] 2023-05-18: This links to `FsConfig`, not `FsUserConfig` as I thought
    // before.
    /// ### feature.fs {#feature-fs}
    #[config(nested, toggleable)]
    pub fs: FsConfig,

    /// ### feature.network {#feature-network}
    #[config(nested, toggleable)]
    pub network: NetworkConfig,

    /// ### feature.copy_target {#feature-copy_target}
    ///
    /// Creates a new copy of the target. mirrord will use this copy instead of the original target
    /// (e.g. intercept network traffic). This feature requires a [mirrord operator](https://metalbear.com/mirrord/docs/overview/teams/?utm_source=copytarget).
    ///
    /// This feature is not compatible with rollout targets and running without a target
    /// (`targetless` mode).
    #[config(nested)]
    pub copy_target: CopyTargetConfig,

    /// ### feature.hostname {#feature-hostname}
    ///
    /// Should mirrord return the hostname of the target pod when calling `gethostname`
    #[config(default = true)]
    pub hostname: bool,

    /// ### feature.split_queues {#feature-split_queues}
    ///
    /// Define filters to split queues by, and make your local application consume only messages
    /// that match those filters.
    /// If you don't specify any filter for a queue that is however declared in the
    /// `MirrordWorkloadQueueRegistry` of the target you're using, a match-nothing filter
    /// will be used, and your local application will not receive any messages from that queue.
    #[config(nested, default, unstable)]
    pub split_queues: SplitQueuesConfig,

    /// ### feature.db_branches {#feature-db_branches}
    ///
    /// Configuration for the database branching feature.
    #[config(nested, default, unstable)]
    pub db_branches: DatabaseBranchesConfig,

    /// ### feature.preview {#feature-preview}
    ///
    /// Configuration for preview environments.
    #[config(nested, default)]
    pub preview: PreviewConfig,
}

impl CollectAnalytics for &FeatureConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("env", &self.env);
        analytics.add("fs", &self.fs);
        analytics.add("network", &self.network);
        analytics.add("copy_target", &self.copy_target);
        analytics.add("hostname", self.hostname);
        analytics.add("split_queues", &self.split_queues);
        analytics.add("db_branches", &self.db_branches);
        analytics.add("preview", &self.preview);
    }
}
