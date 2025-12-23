use std::ops::Deref;

use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{self, source::MirrordConfigSource};

pub mod mysql;
pub mod pg;

pub use mysql::{MysqlBranchConfig, MysqlBranchCopyConfig, MysqlBranchTableCopyConfig};
pub use pg::{PgBranchConfig, PgBranchCopyConfig, PgBranchTableCopyConfig, PgIamAuthConfig};

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

    pub fn count_pg(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Pg { .. }))
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
    Pg(PgBranchConfig),
}

/// Despite the database type, all database branch config objects share the following fields.
#[derive(MirrordConfig, Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[config(map_to = "DatabaseBranchBaseFileConfig")]
pub struct DatabaseBranchBaseConfig {
    /// #### feature.db_branches.base.id {#feature-db_branches-base-id}
    ///
    /// Users can choose to specify a unique `id`. This is useful for reusing or sharing
    /// the same database branch among Kubernetes users.
    pub id: Option<String>,

    /// #### feature.db_branches.base.name {#feature-db_branches-base-name}
    ///
    /// When source database connection detail is not accessible to mirrord operator, users
    /// can specify the database `name` so it is included in the connection options mirrord
    /// uses as the override.
    pub name: Option<String>,

    /// #### feature.db_branches.base.ttl_secs {#feature-db_branches-base-ttl_secs}
    ///
    /// Mirrord operator starts counting the TTL when a branch is no longer used by any session.
    /// The time-to-live (TTL) for the branch database is set to 300 seconds by default.
    /// Users can set `ttl_secs` to customize this value according to their need. Please note
    /// that longer TTL paired with frequent mirrord session turnover can result in increased
    /// resource usage. For this reason, branch database TTL caps out at 15 min.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,

    /// #### feature.db_branches.base.creation_timeout_secs {#feature-db_branches-base-creation_timeout_secs}
    ///
    /// The timeout in seconds to wait for a database branch to become ready after creation.
    /// Defaults to 60 seconds. Adjust this value based on your database size and cluster
    /// performance.
    #[serde(default = "default_creation_timeout_secs")]
    pub creation_timeout_secs: u64,

    /// #### feature.db_branches.base.version {#feature-db_branches-base-version}
    ///
    /// Mirrord operator uses a default version of the database image unless `version` is given.
    pub version: Option<String>,

    /// #### feature.db_branches.base.connection {#feature-db_branches-base-connection}
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
///
/// Support:
/// - `env` in the target's pod spec.
/// - `envFrom` in the target's pod spec.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSourceKind")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionSourceKind {
    Env {
        container: Option<String>,
        variable: String,
    },
    EnvFrom {
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
        analytics.add("pg_branch_count", self.count_pg());
    }
}

fn default_ttl_secs() -> u64 {
    300
}

pub fn default_creation_timeout_secs() -> u64 {
    60
}
