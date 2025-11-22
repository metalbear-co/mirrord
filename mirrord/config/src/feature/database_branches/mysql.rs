use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for MySQL, set `type` to `mysql`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct MysqlBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MysqlBranchCopyConfig,
}

/// Users can choose from the following copy mode to bootstrap their MySQL branch database:
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
pub enum MysqlBranchCopyConfig {
    Empty {
        tables: Option<HashMap<String, MysqlBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<HashMap<String, MysqlBranchTableCopyConfig>>,
    },

    All,
}

impl Default for MysqlBranchCopyConfig {
    fn default() -> Self {
        MysqlBranchCopyConfig::Empty {
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
///     "filter": "my_db.users.name = 'alice' OR my_db.users.name = 'bob'"
///   },
///   "orders": {
///     "filter": "my_db.orders.created_at > 1759948761"
///   }
/// }
/// ```
///
/// With the config above, only alice and bob from the `users` table and orders
/// created after the given timestamp will be copied.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct MysqlBranchTableCopyConfig {
    pub filter: Option<String>,
}
