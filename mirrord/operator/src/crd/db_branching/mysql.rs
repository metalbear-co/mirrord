use std::collections::BTreeMap;

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
    pub copy: BranchCopyConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCopyConfig {
    /// The default copy mode for the branch.
    pub mode: BranchCopyMode,

    /// An optional list of tables whose schema and data will be copied based on their
    /// table level copy config. Only compatible with `Empty` and `Schema` copy mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tables: Option<BTreeMap<String, TableCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database with all tables' schema copied from the source database.
    Schema,
    /// Create a database and copy all tables' schema and data from the source database.
    /// With this copy mode, all table specific copy configs are ignored.
    All,
}

impl Default for BranchCopyConfig {
    fn default() -> Self {
        BranchCopyConfig {
            mode: BranchCopyMode::Empty,
            tables: Default::default(),
        }
    }
}

impl From<MysqlBranchCopyConfig> for BranchCopyConfig {
    fn from(config: MysqlBranchCopyConfig) -> Self {
        match config {
            MysqlBranchCopyConfig::Empty { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                tables: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MysqlBranchCopyConfig::Schema { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Schema,
                tables: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MysqlBranchCopyConfig::All => BranchCopyConfig {
                mode: BranchCopyMode::All,
                tables: None,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TableCopyConfig {
    /// Data that matches the filter will be copied.
    /// For MySQL, this filter is a `where` clause that looks like `username = 'alice'`.
    pub filter: Option<String>,
}

impl From<MysqlBranchTableCopyConfig> for TableCopyConfig {
    fn from(config: MysqlBranchTableCopyConfig) -> Self {
        TableCopyConfig {
            filter: config.filter,
        }
    }
}
