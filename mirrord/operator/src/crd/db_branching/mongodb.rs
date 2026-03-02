use kube::CustomResource;
use mirrord_config::feature::database_branches::{
    MongodbBranchCollectionCopyConfig, MongodbBranchCopyConfig,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
use crate::crd::db_branching::unified::{
    AllOrNothingBranchCopyConfig, AllOrNothingBranchCopyMode, OldCommonFields, TableCopyConfig,
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
    #[serde(flatten)]
    pub common: OldCommonFields,
    pub mongodb_version: Option<String>,
    pub copy: AllOrNothingBranchCopyConfig,
}

impl From<MongodbBranchCopyConfig> for AllOrNothingBranchCopyConfig {
    fn from(config: MongodbBranchCopyConfig) -> Self {
        match config {
            MongodbBranchCopyConfig::Empty { collections } => AllOrNothingBranchCopyConfig {
                mode: AllOrNothingBranchCopyMode::Empty,
                collections: collections.map(|c| {
                    c.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MongodbBranchCopyConfig::All { collections } => AllOrNothingBranchCopyConfig {
                mode: AllOrNothingBranchCopyMode::All,
                collections: collections.map(|c| {
                    c.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
        }
    }
}

impl From<MongodbBranchCollectionCopyConfig> for TableCopyConfig {
    fn from(config: MongodbBranchCollectionCopyConfig) -> Self {
        Self {
            filter: config.filter,
        }
    }
}
