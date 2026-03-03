use std::collections::BTreeMap;

use kube::CustomResource;
use mirrord_config::feature::database_branches::{
    BranchItemCopyConfig, MongodbBranchCopyConfig, MysqlBranchCopyConfig, PgBranchCopyConfig,
    PgIamAuthConfig,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::session::SessionTarget;

pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind,
    IamAuthConfig, SessionInfo,
};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "BranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct BranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// The database engine to provision.
    pub dialect: DatabaseDialect,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// Database name.
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: SessionTarget,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// Database server image version, e.g. "16" for PostgreSQL, "8.0" for MySQL, "7.0" for
    /// MongoDB.
    pub version: Option<String>,
    /// Options for copying data from source database to the branch.
    #[serde(default)]
    pub copy: BranchCopyConfig,
    /// Engine-specific configuration options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect_options: Option<DialectOptions>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseDialect {
    Postgres,
    Mysql,
    Mongodb,
}

impl std::fmt::Display for DatabaseDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseDialect::Postgres => write!(f, "PostgreSQL"),
            DatabaseDialect::Mysql => write!(f, "MySQL"),
            DatabaseDialect::Mongodb => write!(f, "MongoDB"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DialectOptions {
    /// IAM authentication configuration for the source database.
    /// Currently only used by PostgreSQL with AWS RDS / GCP Cloud SQL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCopyConfig {
    /// The copy mode for the branch.
    pub mode: BranchCopyMode,

    /// An optional map of items (tables for SQL databases, collections for MongoDB) to copy,
    /// each with an optional filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database with all tables' schema copied from the source database.
    /// Valid for relational databases (PostgreSQL, MySQL) only.
    Schema,
    /// Create a database and copy all data from the source database.
    All,
}

impl Default for BranchCopyConfig {
    fn default() -> Self {
        BranchCopyConfig {
            mode: BranchCopyMode::Empty,
            items: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCopyConfig {
    /// Data that matches the filter will be copied.
    /// For SQL databases, this is a WHERE clause like `username = 'alice'`.
    /// For MongoDB, this is a JSON query document like `{"username": "alice"}`.
    pub filter: Option<String>,
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

impl From<PgBranchCopyConfig> for BranchCopyConfig {
    fn from(config: PgBranchCopyConfig) -> Self {
        match config {
            PgBranchCopyConfig::Empty { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
            },
            PgBranchCopyConfig::Schema { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
            },
            PgBranchCopyConfig::All => BranchCopyConfig {
                mode: BranchCopyMode::All,
                items: None,
            },
        }
    }
}

impl From<MysqlBranchCopyConfig> for BranchCopyConfig {
    fn from(config: MysqlBranchCopyConfig) -> Self {
        match config {
            MysqlBranchCopyConfig::Empty { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
            },
            MysqlBranchCopyConfig::Schema { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
            },
            MysqlBranchCopyConfig::All => BranchCopyConfig {
                mode: BranchCopyMode::All,
                items: None,
            },
        }
    }
}

impl From<MongodbBranchCopyConfig> for BranchCopyConfig {
    fn from(config: MongodbBranchCopyConfig) -> Self {
        match config {
            MongodbBranchCopyConfig::Empty { collections } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                items: convert_item_copy_configs(collections),
            },
            MongodbBranchCopyConfig::All { collections } => BranchCopyConfig {
                mode: BranchCopyMode::All,
                items: convert_item_copy_configs(collections),
            },
        }
    }
}

fn convert_item_copy_configs(
    items: Option<BTreeMap<String, BranchItemCopyConfig>>,
) -> Option<BTreeMap<String, ItemCopyConfig>> {
    items.map(|m| {
        m.into_iter()
            .map(|(name, config)| (name, ItemCopyConfig::from(config)))
            .collect()
    })
}

impl From<BranchItemCopyConfig> for ItemCopyConfig {
    fn from(config: BranchItemCopyConfig) -> Self {
        ItemCopyConfig {
            filter: config.filter,
        }
    }
}
