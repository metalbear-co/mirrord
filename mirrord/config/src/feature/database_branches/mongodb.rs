use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for MongoDB, set `type` to `mongodb`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
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
/// - Schema
///
///   Creates an empty database and copies schema (indexes) of all collections.
///
/// - All
///
///   Copies both schema and data of all collections. This option shall only be used
///   when the data volume of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum MongodbBranchCopyConfig {
    Empty {
        collections: Option<HashMap<String, MongodbBranchCollectionCopyConfig>>,
    },

    Schema {
        collections: Option<HashMap<String, MongodbBranchCollectionCopyConfig>>,
    },

    All,
}

impl Default for MongodbBranchCopyConfig {
    fn default() -> Self {
        MongodbBranchCopyConfig::Empty {
            collections: Default::default(),
        }
    }
}

/// In addition to copying an empty database or all collections' schema, mirrord operator
/// will copy data from the source DB when an array of collection configs are specified.
///
/// Example:
///
/// ```json
/// {
///   "users": {
///     "filter": "{\"name\": {\"$in\": [\"alice\", \"bob\"]}}"
///   },
///   "orders": {
///     "filter": "{\"created_at\": {\"$gt\": 1759948761}}"
///   }
/// }
/// ```
///
/// With the config above, only alice and bob from the `users` collection and orders
/// created after the given timestamp will be copied.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct MongodbBranchCollectionCopyConfig {
    /// A MongoDB query filter in JSON format. Documents matching this filter will be copied.
    pub filter: Option<String>,
}

