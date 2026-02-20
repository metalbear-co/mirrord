use std::collections::BTreeMap;

use kube::CustomResource;
use mirrord_config::{
    feature::database_branches::{PgBranchCopyConfig, PgBranchTableCopyConfig, PgIamAuthConfig},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind,
    IamAuthConfig, SessionInfo,
};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "PgBranchDatabase",
    status = "BranchDatabaseStatus",
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
    /// IAM authentication configuration for the source database.
    /// Use this when the source database (RDS, Cloud SQL) requires IAM auth instead of passwords.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

impl From<&PgIamAuthConfig> for IamAuthConfig {
    fn from(config: &PgIamAuthConfig) -> Self {
        match config {
            PgIamAuthConfig::AwsRds {
                region,
                access_key_id,
                secret_access_key,
                session_token,
            } => IamAuthConfig::AwsRds {
                region: region.as_ref().map(Into::into),
                access_key_id: access_key_id.as_ref().map(Into::into),
                secret_access_key: secret_access_key.as_ref().map(Into::into),
                session_token: session_token.as_ref().map(Into::into),
            },
            PgIamAuthConfig::GcpCloudSql {
                credentials_json,
                credentials_path,
                project,
            } => IamAuthConfig::GcpCloudSql {
                credentials_json: credentials_json.as_ref().map(Into::into),
                credentials_path: credentials_path.as_ref().map(Into::into),
                project: project.as_ref().map(Into::into),
            },
        }
    }
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
