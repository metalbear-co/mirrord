use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Configuration for local Redis.
///
/// When enabled, mirrord will spawn a local Redis container.
/// Use with `feature.network.outgoing.filter.local` to redirect traffic from the remote
/// Redis hostname to your local instance.
///
/// ```json
/// {
///   "feature": {
///     "redis": {
///       "local": true,
///       "local_port": 6379
///     },
///     "network": {
///       "outgoing": {
///         "filter": {
///           "local": ["redis-main:6379"]
///         },
///         "ignore_localhost": true
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[config(map_to = "RedisFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct RedisConfig {
    /// ### feature.redis.local {#feature-redis-local}
    ///
    /// When enabled, mirrord will spawn a local Redis container.
    /// The application's Redis connection will be automatically redirected to this local instance.
    ///
    /// Defaults to `false`.
    #[config(default = false)]
    pub local: bool,

    /// ### feature.redis.local_port {#feature-redis-local_port}
    ///
    /// Port for the local Redis when `local` is enabled.
    /// Useful if the default port 6379 is already in use.
    ///
    /// Defaults to `6379`.
    #[config(default = 6379)]
    pub local_port: u16,
}

impl CollectAnalytics for &RedisConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("local", self.local);
    }
}
