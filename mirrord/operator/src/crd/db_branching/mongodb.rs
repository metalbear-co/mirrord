use kube::CustomResource;
use mirrord_config::{
    feature::database_branches::{MongodbBranchCollectionCopyConfig, MongodbBranchCopyConfig},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
use super::unified::{
    ItemCopyConfig, MongodbBranchCopyConfig as UnifiedMongodbCopyConfig, MongodbBranchCopyMode,
};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "MongodbBranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MongodbBranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// MongoDB database name.
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: Target,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// MongoDB server image version, e.g. "7.0".
    pub mongodb_version: Option<String>,
    /// Options for copying data from source database to the branch.
    #[serde(default)]
    pub copy: UnifiedMongodbCopyConfig,
}

impl From<MongodbBranchCopyConfig> for UnifiedMongodbCopyConfig {
    fn from(config: MongodbBranchCopyConfig) -> Self {
        match config {
            MongodbBranchCopyConfig::Empty { collections } => UnifiedMongodbCopyConfig {
                mode: MongodbBranchCopyMode::Empty,
                items: collections.map(|c| {
                    c.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MongodbBranchCopyConfig::All { collections } => UnifiedMongodbCopyConfig {
                mode: MongodbBranchCopyMode::All,
                items: collections.map(|c| {
                    c.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
        }
    }
}

impl From<MongodbBranchCollectionCopyConfig> for ItemCopyConfig {
    fn from(config: MongodbBranchCollectionCopyConfig) -> Self {
        Self {
            filter: config.filter,
        }
    }
}
