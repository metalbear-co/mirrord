use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for MongoDB, set `type` to `mongodb`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MongodbBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MongodbBranchCopyConfig,
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
