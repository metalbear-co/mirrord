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
use super::unified::{ItemCopyConfig, SqlBranchCopyConfig, SqlBranchCopyMode};

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
    pub copy: SqlBranchCopyConfig,
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

impl From<PgBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: PgBranchCopyConfig) -> Self {
        match config {
            PgBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            PgBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            PgBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
            },
        }
    }
}

impl From<PgBranchTableCopyConfig> for ItemCopyConfig {
    fn from(config: PgBranchTableCopyConfig) -> Self {
        Self {
            filter: config.filter,
        }
    }
}
