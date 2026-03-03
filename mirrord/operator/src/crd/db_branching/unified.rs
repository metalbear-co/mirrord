use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::core::IamAuthConfig;
pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
use crate::crd::session::SessionTarget;

/// Unified branch database CRD. Uses an internally tagged enum so each
/// variant (`mysql`, `postgres`, `mongodb`) carries only its applicable fields.
/// The tag field in the serialized JSON/YAML is `dbType`.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "BranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "kebab-case", tag = "dbType")]
#[allow(clippy::large_enum_variant)]
pub enum BranchDatabaseSpec {
    Mysql {
        /// ID derived by mirrord CLI.
        id: String,
        /// Database connection info from the workload.
        #[serde(rename = "connectionSource")]
        connection_source: ConnectionSource,
        /// Database name.
        #[serde(rename = "databaseName")]
        database_name: Option<String>,
        /// Target k8s resource to extract connection source info from.
        target: SessionTarget,
        /// The duration in seconds this branch database will live idling.
        #[serde(rename = "ttlSecs")]
        ttl_secs: u64,
        /// Database server image version, e.g. "8.0" for MySQL.
        #[serde(rename = "imageVersion")]
        image_version: Option<String>,
        /// Options for copying data from source database to the branch.
        #[serde(default)]
        copy: SqlBranchCopyConfig,
    },
    Postgres {
        /// ID derived by mirrord CLI.
        id: String,
        /// Database connection info from the workload.
        #[serde(rename = "connectionSource")]
        connection_source: ConnectionSource,
        /// Database name.
        #[serde(rename = "databaseName")]
        database_name: Option<String>,
        /// Target k8s resource to extract connection source info from.
        target: SessionTarget,
        /// The duration in seconds this branch database will live idling.
        #[serde(rename = "ttlSecs")]
        ttl_secs: u64,
        /// Database server image version, e.g. "16" for PostgreSQL.
        #[serde(rename = "imageVersion")]
        image_version: Option<String>,
        /// Options for copying data from source database to the branch.
        #[serde(default)]
        copy: SqlBranchCopyConfig,
        /// IAM authentication configuration for the source database.
        /// Use this when the source database (RDS, Cloud SQL) requires IAM auth instead of
        /// passwords.
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "iamAuth")]
        iam_auth: Option<IamAuthConfig>,
    },
    Mongodb {
        /// ID derived by mirrord CLI.
        id: String,
        /// Database connection info from the workload.
        #[serde(rename = "connectionSource")]
        connection_source: ConnectionSource,
        /// Database name.
        #[serde(rename = "databaseName")]
        database_name: Option<String>,
        /// Target k8s resource to extract connection source info from.
        target: SessionTarget,
        /// The duration in seconds this branch database will live idling.
        #[serde(rename = "ttlSecs")]
        ttl_secs: u64,
        /// Database server image version, e.g. "7.0" for MongoDB.
        #[serde(rename = "imageVersion")]
        image_version: Option<String>,
        /// Options for copying data from source database to the branch.
        #[serde(default)]
        copy: MongodbBranchCopyConfig,
    },
}

/// Read-only view of the common fields shared by all variants.
pub struct CommonFieldsRef<'a> {
    pub id: &'a str,
    pub connection_source: &'a ConnectionSource,
    pub database_name: Option<&'a str>,
    pub target: &'a SessionTarget,
    pub ttl_secs: u64,
    pub image_version: Option<&'a str>,
}

impl BranchDatabaseSpec {
    pub fn common(&self) -> CommonFieldsRef<'_> {
        match self {
            Self::Mysql {
                id,
                connection_source,
                database_name,
                target,
                ttl_secs,
                image_version,
                ..
            }
            | Self::Postgres {
                id,
                connection_source,
                database_name,
                target,
                ttl_secs,
                image_version,
                ..
            }
            | Self::Mongodb {
                id,
                connection_source,
                database_name,
                target,
                ttl_secs,
                image_version,
                ..
            } => CommonFieldsRef {
                id,
                connection_source,
                database_name: database_name.as_deref(),
                target,
                ttl_secs: *ttl_secs,
                image_version: image_version.as_deref(),
            },
        }
    }
}

/// Copy configuration for SQL databases (PostgreSQL, MySQL).
/// Supports `Empty`, `Schema`, and `All` copy modes with optional per-table filters.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SqlBranchCopyConfig {
    /// The copy mode for the branch.
    pub mode: SqlBranchCopyMode,

    /// An optional map of tables to copy with per-table filter configuration.
    /// Only compatible with `Empty` and `Schema` copy modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SqlBranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database with all tables' schema copied from the source database.
    Schema,
    /// Create a database and copy all tables' schema and data from the source database.
    /// With this copy mode, all item-specific copy configs are ignored.
    All,
}

impl Default for SqlBranchCopyConfig {
    fn default() -> Self {
        Self {
            mode: SqlBranchCopyMode::Empty,
            items: None,
        }
    }
}

/// Copy configuration for MongoDB.
/// Supports `Empty` and `All` copy modes with optional per-collection filters.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbBranchCopyConfig {
    /// The copy mode for the branch.
    pub mode: MongodbBranchCopyMode,

    /// An optional map of collections to copy with per-collection filter configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MongodbBranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database and copy all collections and their data from the source database.
    /// Supports optional collection filters.
    All,
}

impl Default for MongodbBranchCopyConfig {
    fn default() -> Self {
        Self {
            mode: MongodbBranchCopyMode::Empty,
            items: None,
        }
    }
}

/// Per-item (table or collection) copy configuration.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCopyConfig {
    /// Data that matches the filter will be copied.
    /// For SQL databases this is a WHERE clause (e.g. `username = 'alice'`).
    /// For MongoDB this is a JSON query document (e.g. `{"username": "alice"}`).
    pub filter: Option<String>,
}
