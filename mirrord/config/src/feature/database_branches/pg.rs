use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for PostgreSQL, set `type` to `pg`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct PgBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: PgBranchCopyConfig,
}

/// Users can choose from the following copy mode to bootstrap their PostgreSQL branch database:
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
///   Creates an empty database and copies schema of all tables.
///
/// - All
///
///   Copies both schema and data of all tables. This option shall only be used
///   when the data volume of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum PgBranchCopyConfig {
    Empty {
        tables: Option<HashMap<String, PgBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<HashMap<String, PgBranchTableCopyConfig>>,
    },

    All,
}

impl Default for PgBranchCopyConfig {
    fn default() -> Self {
        PgBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

/// In addition to copying an empty database or all tables' schema, mirrord operator
/// will copy data from the source DB when an array of table configs are specified.
///
/// Example:
///
/// ```json
/// {
///   "users": {
///     "filter": "name = 'alice' OR name = 'bob'"
///   },
///   "orders": {
///     "filter": "created_at > '2025-01-01'"
///   }
/// }
/// ```
///
/// With the config above, only alice and bob from the `users` table and orders
/// created after the given timestamp will be copied.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct PgBranchTableCopyConfig {
    pub filter: Option<String>,
}
