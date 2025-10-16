use std::{collections::HashMap, ops::Deref};

use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{self, source::MirrordConfigSource};

/// A list of configurations for database branches.
///
/// ```json
/// {
///   "feature": {
///     "db_branches": [
///       {
///         "name": "my-database-name",
///         "ttl_secs": 120,
///         "type": "mysql",
///         "version": "8.0",
///         "connection": {
///           "url": {
///             "type": "env",
///             "variable": "DB_CONNECTION_URL"
///           }
///         }
///       }
///     ]
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
pub struct DatabaseBranchesConfig(pub Vec<DatabaseBranchConfig>);

impl Deref for DatabaseBranchesConfig {
    type Target = Vec<DatabaseBranchConfig>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DatabaseBranchesConfig {
    pub fn count_mysql(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Mysql { .. }))
            .count()
    }
}

/// Configuration for a database branch.
///
/// Example:
///
/// ```json
/// {
///   "id": "my-branch-db",
///   "name": "my-database-name",
///   "ttl_secs": 120,
///   "type": "mysql",
///   "version": "8.0",
///   "connection": {
///     "url": {
///       "type": "env",
///       "variable": "DB_CONNECTION_URL"
///     }
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DatabaseBranchConfig {
    Mysql(MysqlBranchConfig),
}

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
/// Create an empty database. If the source DB connection options are found from the chosen
/// target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
/// operator looks for the `name` field from the branch DB config object. This option is useful for
/// users that run DB migrations themselves before starting the application.
///
/// - Schema
///
/// Create an empty database and copy schema of all tables.
///
/// - Data
///
/// Copy both schema and data of all tables. This option shall only be used
/// when the data volume of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum MysqlBranchCopyConfig {
    Empty {
        tables: Option<HashMap<String, MysqlBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<HashMap<String, MysqlBranchTableCopyConfig>>,
    },

    Data,
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
///     "filter": "WHERE my_db.users.name = 'alice' OR my_db.users.name = 'bob'"
///   },
///   "orders": {
///     "filter": "WHERE my_db.orders.created_at > 1759948761"
///   }
/// }
/// ```
///
/// With the config above, only alice and bob from the `users` table and orders
/// created after the given timestamp will be copied.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct MysqlBranchTableCopyConfig {
    pub filter: String,
}

/// Despite the database type, all database branch config objects share the following fields.
#[derive(MirrordConfig, Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[config(map_to = "DatabaseBranchBaseFileConfig")]
pub struct DatabaseBranchBaseConfig {
    /// ### feature.db_branches.base.id {#feature-db_branches-base-id}
    ///
    /// Users can choose to specify a unique `id`. This is useful for reusing or sharing
    /// the same database branch among Kubernetes users.
    pub id: Option<String>,

    /// ### feature.db_branches.base.name {#feature-db_branches-base-name}
    ///
    /// When source database connection detail is not accessible to mirrord operator, users
    /// can specify the database `name` so it is included in the connection options mirrord
    /// uses as the override.
    pub name: Option<String>,

    /// ### feature.db_branches.base.ttl_secs {#feature-db_branches-base-ttl_secs}
    ///
    /// Mirrord operator starts counting the TTL when a branch is no longer used by any session.
    /// The time-to-live (TTL) for the branch database is set to 300 seconds by default.
    /// Users can set `ttl_secs` to customize this value according to their need. Please note
    /// that longer TTL paired with frequent mirrord session turnover can result in increased
    /// resource usage. For this reason, branch database TTL caps out at 15 min.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,

    /// ### feature.db_branches.base.version {#feature-db_branches-base-version}
    ///
    /// Mirrord operator uses a default version of the database image unless `version` is given.
    pub version: Option<String>,

    /// ### feature.db_branches.base.connection {#feature-db_branches-base-connection}
    ///
    /// `connection` describes how to get the connection information to the source database.
    /// When the branch database is ready for use, Mirrord operator will replace the connection
    /// information with the branch database's.
    pub connection: ConnectionSource,
}

/// Different ways of connecting to the source database.
///
/// Example:
///
/// A single complete connection URL stored in an environment variable accessible from
/// the target pod template.
///
/// ```json
/// {
///   "url": {
///     "type": "env",
///     "variable": "DB_CONNECTION_URL"
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSource")]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSource {
    Url(ConnectionSourceKind),
}

/// Different ways to source the connection options.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSourceKind")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionSourceKind {
    Env {
        container: Option<String>,
        variable: String,
    },
}

impl config::MirrordConfig for DatabaseBranchesConfig {
    type Generated = Self;

    fn generate_config(
        self,
        _context: &mut config::ConfigContext,
    ) -> crate::config::Result<Self::Generated> {
        Ok(self)
    }
}

impl config::FromMirrordConfig for DatabaseBranchesConfig {
    type Generator = Self;
}

impl CollectAnalytics for &DatabaseBranchesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("mysql_branch_count", self.count_mysql());
    }
}

fn default_ttl_secs() -> u64 {
    300
}
