use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for Redis, set `type` to `redis`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct RedisBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    /// Where the Redis instance should run.
    /// - `local`: Spawns a local Redis container.
    /// - `remote`: Uses the remote Redis (default behavior).
    #[serde(default)]
    pub location: RedisBranchLocation,
}

/// Location for the Redis branch instance.
///
/// - `local`: Spawns a local Redis container that mirrord manages.
/// - `remote`: Uses the remote Redis instance
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RedisBranchLocation {
    /// Use a local Redis container.
    Local,
    /// Use the remote Redis (default behavior, no-op).
    #[default]
    Remote,
}
