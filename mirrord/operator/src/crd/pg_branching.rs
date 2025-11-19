use std::{
    collections::{BTreeMap, HashMap},
    fmt::Formatter,
};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use mirrord_config::{
    feature::database_branches::{PgBranchCopyConfig, PgBranchTableCopyConfig},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::session::SessionOwner;

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "PgBranchDatabase",
    status = "PgBranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct PgBranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// PostgreSQL database name.
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: Target,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// PostgreSQL server image version, e.g. "16".
    pub postgres_version: Option<String>,
    /// Options for copying data from source database to the branch.
    #[serde(default)]
    pub copy: BranchCopyConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSource {
    /// A complete connection URL.
    Url(ConnectionSourceKind),
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSourceKind {
    /// Environment variable with value defined directly in the pod template.
    Env {
        container: Option<String>,
        variable: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PgBranchDatabaseStatus {
    pub phase: BranchDatabasePhase,
    /// Time when the branch database should be deleted.
    pub expire_time: MicroTime,
    /// Information of sessions that are using this branch database.
    #[serde(default)]
    pub session_info: HashMap<String, SessionInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum BranchDatabasePhase {
    /// The controller is creating the branch database.
    Pending,
    /// The branch database is ready to use.
    Ready,
}

impl std::fmt::Display for BranchDatabasePhase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabasePhase::Pending => write!(f, "Pending"),
            BranchDatabasePhase::Ready => write!(f, "Ready"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Owner info of the session.
    pub owner: SessionOwner,
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

impl From<PgBranchCopyConfig> for BranchCopyConfig {
    fn from(config: PgBranchCopyConfig) -> Self {
        match config {
            PgBranchCopyConfig::Empty { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                tables: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            PgBranchCopyConfig::Schema { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Schema,
                tables: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            PgBranchCopyConfig::All => BranchCopyConfig {
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
    /// For PostgreSQL, this filter is a `WHERE` clause that looks like `username = 'alice'`.
    pub filter: Option<String>,
}

impl From<PgBranchTableCopyConfig> for TableCopyConfig {
    fn from(config: PgBranchTableCopyConfig) -> Self {
        TableCopyConfig {
            filter: config.filter,
        }
    }
}
