use std::ops::Deref;

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
    pub fn mysql(&self) -> impl '_ + Iterator<Item = &DatabaseBranchConfig> {
        self.0.iter().filter(|db| db._type == DatabaseType::MySql)
    }
}

/// Configuration for a database branch.
///
/// By specifying a unique `id`, the branch database will be reused if not expired.
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
#[derive(MirrordConfig, Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[config(map_to = "DatabaseBranchFileConfig")]
pub struct DatabaseBranchConfig {
    /// <!--${internal}-->
    /// A unique identifier chosen by the user. This is useful when reusing branch database
    /// sharing among different Kubernetes users.
    pub id: Option<String>,

    /// <!--${internal}-->
    /// Name of the database
    pub name: Option<String>,

    /// <!--${internal}-->
    /// The time-to-live (TTL) for the branch database, in seconds. Defaults to 300 seconds.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,

    /// <!--${internal}-->
    /// The database type.
    #[serde(rename = "type")]
    pub _type: DatabaseType,

    /// <!--${internal}-->
    /// The database image version.
    pub version: Option<String>,

    /// <!--${internal}-->
    /// The database connection details.
    pub connection: ConnectionSource,
}

/// <!--${internal}-->
/// Supported database types.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingDatabaseType")]
pub enum DatabaseType {
    #[serde(rename = "mysql")]
    MySql,
}

/// <!--${internal}-->
/// Options for connecting to the database.
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
        analytics.add("mysql_branch_count", self.mysql().count());
    }
}

fn default_ttl_secs() -> u64 {
    300
}
