use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DatabaseBranchBaseConfig, IamAuthConfig};

/// When configuring a branch for DynamoDB, set `type` to `dynamodb`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamodbBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: DynamodbBranchCopyConfig,

    /// #### feature.db_branches[].iam_auth (type: dynamodb) {#feature-db_branches-dynamodb-iam_auth}
    ///
    /// AWS IAM credentials used to read the source DynamoDB tables. DynamoDB has no
    /// password-based connection mode, so this is the only way to authenticate against the
    /// source when `copy.mode` is `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

/// Users can choose from the following copy mode to bootstrap their DynamoDB branch database:
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
pub enum DynamodbBranchCopyConfig {
    Empty {
        collections: Option<BTreeMap<String, DynamodbBranchCollectionCopyConfig>>,
    },

    All {
        /// Optional collection filters. If not specified, all collections are copied.
        /// If specified, only the listed collections are copied with their optional filters.
        collections: Option<BTreeMap<String, DynamodbBranchCollectionCopyConfig>>,
    },
}

impl Default for DynamodbBranchCopyConfig {
    fn default() -> Self {
        DynamodbBranchCopyConfig::Empty {
            collections: Default::default(),
        }
    }
}

pub type DynamodbBranchCollectionCopyConfig = super::BranchItemCopyConfig;
