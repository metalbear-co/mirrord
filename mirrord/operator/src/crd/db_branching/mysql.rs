use kube::CustomResource;
use mirrord_config::{
    feature::database_branches::{MysqlBranchCopyConfig, MysqlBranchTableCopyConfig},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
use super::unified::{ItemCopyConfig, SqlBranchCopyConfig, SqlBranchCopyMode};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "MysqlBranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MysqlBranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// MySQL database name.
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: Target,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// MySQL server image version, e.g. "8.0".
    pub mysql_version: Option<String>,
    /// Options for copying data from source database to the branch.
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
}

impl From<MysqlBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: MysqlBranchCopyConfig) -> Self {
        match config {
            MysqlBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MysqlBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MysqlBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
            },
        }
    }
}

impl From<MysqlBranchTableCopyConfig> for ItemCopyConfig {
    fn from(config: MysqlBranchTableCopyConfig) -> Self {
        Self {
            filter: config.filter,
        }
    }
}
