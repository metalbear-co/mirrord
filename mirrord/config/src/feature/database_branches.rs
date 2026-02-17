use std::{collections::HashMap, ops::Deref};

use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{self, source::MirrordConfigSource};

pub mod mongodb;
pub mod mysql;
pub mod pg;
pub mod redis;

pub use mongodb::{
    MongodbBranchCollectionCopyConfig, MongodbBranchConfig, MongodbBranchCopyConfig,
};
pub use mysql::{MysqlBranchConfig, MysqlBranchCopyConfig, MysqlBranchTableCopyConfig};
pub use pg::{PgBranchConfig, PgBranchCopyConfig, PgBranchTableCopyConfig, PgIamAuthConfig};
pub use redis::{
    RedisBranchConfig, RedisBranchLocation, RedisConnectionConfig, RedisLocalConfig, RedisOptions,
    RedisRuntime, RedisValueSource,
};

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
    pub fn count_mongodb(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Mongodb { .. }))
            .count()
    }

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

    pub fn count_redis(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Redis { .. }))
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
    Mongodb(MongodbBranchConfig),
    Mysql(MysqlBranchConfig),
    Pg(PgBranchConfig),
    Redis(RedisBranchConfig),
}

/// MySQL and Postgres database branch config objects share the following fields.
#[derive(MirrordConfig, Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[config(map_to = "DatabaseBranchBaseFileConfig")]
pub struct DatabaseBranchBaseConfig {
    /// #### feature.db_branches[].id (type: mysql, pg, mongodb) {#feature-db_branches-sql-id}
    ///
    /// Users can choose to specify a unique `id`. This is useful for reusing or sharing
    /// the same database branch among Kubernetes users.
    pub id: Option<String>,

    /// #### feature.db_branches[].name (type: mysql, pg, mongodb) {#feature-db_branches-sql-name}
    ///
    /// When source database connection detail is not accessible to mirrord operator, users
    /// can specify the database `name` so it is included in the connection options mirrord
    /// uses as the override.
    pub name: Option<String>,

    /// #### feature.db_branches[].ttl_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-ttl_secs}
    ///
    /// Mirrord operator starts counting the TTL when a branch is no longer used by any session.
    /// The time-to-live (TTL) for the branch database is set to 300 seconds by default.
    /// Users can set `ttl_secs` to customize this value according to their need. Please note
    /// that longer TTL paired with frequent mirrord session turnover can result in increased
    /// resource usage. For this reason, branch database TTL caps out at 15 min.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,

    /// #### feature.db_branches[].creation_timeout_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-creation_timeout_secs}
    ///
    /// The timeout in seconds to wait for a database branch to become ready after creation.
    /// Defaults to 60 seconds. Adjust this value based on your database size and cluster
    /// performance.
    #[serde(default = "default_creation_timeout_secs")]
    pub creation_timeout_secs: u64,

    /// #### feature.db_branches[].version (type: mysql, pg, mongodb) {#feature-db_branches-sql-version}
    ///
    /// Mirrord operator uses a default version of the database image unless `version` is given.
    pub version: Option<String>,

    /// #### feature.db_branches[].connection (type: mysql, pg, mongodb) {#feature-db_branches-sql-connection}
    ///
    /// `connection` describes how to get the connection information to the source database.
    /// When the branch database is ready for use, Mirrord operator will replace the connection
    /// information with the branch database's.
    pub connection: ConnectionSource,
}

/// Different ways of connecting to the source database.
///
/// Supports two formats:
///
/// **URL format** (old) — a single complete connection URL from an environment variable:
///
/// ```json
/// {
///   "url": {
///     "type": "env",
///     "variable": "DB_CONNECTION_URL"
///   }
/// }
/// ```
///
/// **Params format** (new) — individual connection parameters from separate env vars:
///
/// ```json
/// {
///   "type": "env",
///   "params": {
///     "host": "DB_HOST",
///     "port": "DB_PORT",
///     "user": "DB_USER",
///     "password": "DB_PASSWORD",
///     "database": "DB_NAME"
///   }
/// }
/// ```
///
/// `url` and `params` are mutually exclusive.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSource")]
#[serde(untagged)]
pub enum ConnectionSource {
    /// Old format: `{"url": {"type": "env", "variable": "..."}}`.
    Url(UrlConnectionSource),
    /// New format: `{"type": "env", "url": "..."}` or `{"type": "env", "params": {...}}`.
    Flat(FlatConnectionSource),
}

/// Old-format wrapper: `{"url": <TargetEnviromentVariableSource>}`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingUrlConnectionSource")]
pub struct UrlConnectionSource {
    pub url: TargetEnviromentVariableSource,
}

/// New flat format: `{"type": "env", "url": "VAR"}` or `{"type": "env", "params": {...}}`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingFlatConnectionSource")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FlatConnectionSource {
    Env {
        /// Complete connection URL env var name. Mutually exclusive with `params`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// Individual connection parameter env var names. Mutually exclusive with `url`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<HashMap<String, String>>,
        /// Optional container name to scope the env var lookup.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        container: Option<String>,
    },
}

impl ConnectionSource {
    /// Get the env variable source for URL mode, regardless of format.
    /// Returns `None` if this is a params-mode connection.
    pub fn as_url_source(&self) -> Option<&TargetEnviromentVariableSource> {
        match self {
            ConnectionSource::Url(u) => Some(&u.url),
            ConnectionSource::Flat(FlatConnectionSource::Env { url: Some(_), .. }) => None,
            _ => None,
        }
    }
}

/// <!--${internal}-->
/// Different ways to source the connection options.
///
/// Support:
/// - `env` in the target's pod spec.
/// - `envFrom` in the target's pod spec.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSourceKind")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TargetEnviromentVariableSource {
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
        analytics.add("mongodb_branch_count", self.count_mongodb());
        analytics.add("mysql_branch_count", self.count_mysql());
        analytics.add("pg_branch_count", self.count_pg());
        analytics.add("redis_branch_count", self.count_redis());
    }
}

fn default_ttl_secs() -> u64 {
    300
}

pub fn default_creation_timeout_secs() -> u64 {
    60
}
