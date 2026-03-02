use std::collections::BTreeMap;

use kube::CustomResource;
use mirrord_config::target::Target;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::core::IamAuthConfig;
pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};

/// Common to the old, separate CRDs - Like [`CommonFields`] but without
/// image_version, because that used to be called a different name for each
/// DB type.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct OldCommonFields {
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
}

/// fields that are present in all BranchDatabase CRDs, regardless of the database type.
/// Can be changed later - e.g. if a a new DB type does not have one of those fields,
/// that field can be pulled out into all existing DB types, and not included in
/// a new DB type, without breaking anything.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct CommonFields {
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
    pub image_version: Option<String>,
}

/// New, unified, DB-Branching Branch CRD.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "BranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "kebab-case", tag = "dbType")]
pub enum BranchDatabaseSpec {
    Mysql {
        #[serde(flatten)]
        common: CommonFields,
        copy: BranchCopyConfig,
    },
    Postgres {
        #[serde(flatten)]
        common: CommonFields,
        copy: BranchCopyConfig,
        /// IAM authentication configuration for the source database.
        /// Use this when the source database (RDS, Cloud SQL) requires IAM auth instead of
        /// passwords.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        iam_auth: Option<IamAuthConfig>,
    },
    Mongodb {
        #[serde(flatten)]
        common: CommonFields,
        copy: AllOrNothingBranchCopyConfig,
    },
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

/// For DB types that don't support schema copy mode.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AllOrNothingBranchCopyConfig {
    /// The copy mode for the branch.
    pub mode: AllOrNothingBranchCopyMode,

    /// An optional list of collections to copy with their filters.
    /// If not specified, all collections are copied (for `All` mode) or none (for `Empty` mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collections: Option<BTreeMap<String, TableCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AllOrNothingBranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database and copy collections' schema and data from the source database.
    /// Supports optional collection filters to copy specific collections or filter documents.
    All,
}

impl Default for AllOrNothingBranchCopyConfig {
    fn default() -> Self {
        AllOrNothingBranchCopyConfig {
            mode: AllOrNothingBranchCopyMode::Empty,
            collections: Default::default(),
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
