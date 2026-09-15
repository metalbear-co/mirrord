use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{BranchBaseConfig, BranchPodConfig, DatabaseSourceConfig, IamAuthConfig};

/// When configuring a branch for MongoDB, set `type` to `mongodb`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MongodbBranchConfig {
    #[serde(flatten)]
    pub base: BranchBaseConfig,

    #[serde(flatten)]
    pub pod: BranchPodConfig,

    #[serde(flatten)]
    pub database: DatabaseSourceConfig,

    #[serde(default)]
    pub copy: MongodbBranchCopyConfig,

    /// #### feature.db_branches[].iam_auth (type: mongodb) {#feature-db_branches-mongodb-iam_auth}
    ///
    /// IAM authentication for the source database.
    /// Use this when your source database (e.g. MongoDB Atlas with AWS IAM) requires the
    /// `MONGODB-AWS` authentication mechanism instead of password-based authentication.
    /// Only the `aws_rds` type is supported for MongoDB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

/// Users can choose from the following copy mode to bootstrap their MongoDB branch database:
///
/// - Empty
///
///   Creates an empty database. If the source DB connection options are found from the chosen
///   target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
///   operator looks for the `name` field from the branch DB config object. This option is useful
///   for users that run DB migrations themselves before starting the application.
///
/// - All
///
///   Copies both schema and data of all collections. Supports optional collection filters
///   to copy only specific collections or filter documents within collections.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum MongodbBranchCopyConfig {
    Empty {
        collections: Option<BTreeMap<String, MongodbBranchCollectionCopyConfig>>,
    },

    All {
        /// Optional collection filters. If not specified, all collections are copied.
        /// If specified, only the listed collections are copied with their optional filters.
        collections: Option<BTreeMap<String, MongodbBranchCollectionCopyConfig>>,
    },
}

impl Default for MongodbBranchCopyConfig {
    fn default() -> Self {
        MongodbBranchCopyConfig::Empty {
            collections: Default::default(),
        }
    }
}

pub type MongodbBranchCollectionCopyConfig = super::BranchItemCopyConfig;
