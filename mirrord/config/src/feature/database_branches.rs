use std::ops::Deref;

use mirrord_analytics::{Analytics, CollectAnalytics};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{ConfigContext, FromMirrordConfig, MirrordConfig};

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

/// Configuration for branch databases.
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
pub struct DatabaseBranchConfig {
    /// A unique identifier chosen by the user. This is useful when reusing branch database
    /// sharing among different Kubernetes users.
    pub id: Option<String>,

    /// Name of the database
    pub name: String,

    /// The time-to-live (TTL) for the branch database, in seconds.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: Option<u64>,

    /// The database type.
    #[serde(rename = "type")]
    pub _type: DatabaseType,

    /// The database image version.
    pub version: Option<String>,

    /// The database connection details.
    pub connection: ConnectionSource,
}

/// Supported database types.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub enum DatabaseType {
    #[serde(rename = "mysql")]
    MySql,
}

/// Options for connecting to the database.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSource {
    Url(ConnectionSourceKind),
}

/// Different ways to source the connection options.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionSourceKind {
    Env {
        container: Option<String>,
        variable: String,
    },
}

fn default_ttl_secs() -> Option<u64> {
    Some(60)
}

impl MirrordConfig for DatabaseBranchesConfig {
    type Generated = Self;

    fn generate_config(
        self,
        _context: &mut ConfigContext,
    ) -> crate::config::Result<Self::Generated> {
        Ok(self)
    }
}

impl FromMirrordConfig for DatabaseBranchesConfig {
    type Generator = Self;
}

impl CollectAnalytics for &DatabaseBranchesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("mysql_branch_count", self.mysql().count());
    }
}
