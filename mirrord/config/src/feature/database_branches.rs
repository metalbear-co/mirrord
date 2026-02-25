use std::ops::Deref;

use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, ser::SerializeMap};

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
/// Using a connection URL:
/// ```json
/// {
///   "feature": {
///     "db_branches": [
///       {
///         "type": "mysql",
///         "connection": {
///           "url": { "type": "env", "variable": "DB_CONNECTION_URL" }
///         }
///       }
///     ]
///   }
/// }
/// ```
///
/// Using individual connection params:
/// ```json
/// {
///   "feature": {
///     "db_branches": [
///       {
///         "type": "mysql",
///         "connection": {
///           "type": "env",
///           "params": { "host": "DB_HOST", "port": "DB_PORT", "database": "DB_NAME" }
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
    Mongodb(Box<MongodbBranchConfig>),
    Mysql(Box<MysqlBranchConfig>),
    Pg(Box<PgBranchConfig>),
    Redis(Box<RedisBranchConfig>),
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
/// Accepts three formats:
///
/// Legacy URL (backward compatible):
/// ```json
/// { "url": { "type": "env", "variable": "DB_CONNECTION_URL" } }
/// ```
///
/// Flat URL:
/// ```json
/// { "type": "env", "url": "DB_CONNECTION_URL" }
/// ```
///
/// Individual connection params:
/// ```json
/// { "type": "env", "params": { "host": "DB_HOST", "port": "DB_PORT", "user": "DB_USER", "password": "DB_PASSWORD", "database": "DB_NAME" } }
/// ```
///
/// Individual connection params with password from a Kubernetes Secret:
/// ```json
/// { "type": "env", "params": { "host": "DB_HOST", "password": { "secret": "my-secret", "key": "password" }, "database": "DB_NAME" } }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSource")]
#[serde(untagged)]
pub enum ConnectionSource {
    Url {
        url: TargetEnviromentVariableSource,
    },
    FlatUrl {
        #[serde(rename = "type")]
        source_type: ConnectionSourceType,
        url: String,
    },
    Params(ConnectionParamsConfig),
}

impl Serialize for ConnectionSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Url { url: source } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("url", source)?;
                map.end()
            }
            Self::FlatUrl { source_type, url } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", source_type)?;
                map.serialize_entry("url", url)?;
                map.end()
            }
            Self::Params(config) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", &config.source_type)?;
                map.serialize_entry("params", &config.params)?;
                map.end()
            }
        }
    }
}

/// The type of environment variable source for connection params.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSourceType {
    Env,
    EnvFrom,
}

/// Connection parameters specified as individual environment variable names.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct ConnectionParamsConfig {
    #[serde(rename = "type")]
    pub source_type: ConnectionSourceType,
    pub params: ConnectionParamsVars,
}

/// <!--${internal}-->
/// A connection parameter source: either a plain env var name (string) or a Kubernetes Secret
/// reference (object).
///
/// As a string: `"DB_HOST"` — resolved using the parent `type` field (env or env_from).
///
/// As an object: `{ "secret": "my-secret", "key": "password" }` — read directly from a
/// Kubernetes Secret.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamSource {
    Variable(String),
    Secret {
        #[serde(rename = "secret")]
        name: String,
        key: String,
    },
}

impl ParamSource {
    pub fn as_variable(&self) -> Option<&str> {
        match self {
            Self::Variable(v) => Some(v),
            Self::Secret { .. } => None,
        }
    }
}

/// Individual database connection parameter sources.
/// At least one parameter must be specified.
/// Each parameter is either a plain string (env var name) or an object with `secret` and `key`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct ConnectionParamsVars {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<ParamSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<ParamSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<ParamSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<ParamSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<ParamSource>,
}

/// <!--${internal}-->
/// Different ways to source the connection options.
///
/// Support:
/// - `env` in the target's pod spec.
/// - `envFrom` in the target's pod spec.
/// - `secret` read directly from a Kubernetes Secret.
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
    Secret {
        name: String,
        key: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_legacy_url_env() {
        let json = r#"{ "url": { "type": "env", "variable": "DB_URL" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::Url {
                url: TargetEnviromentVariableSource::Env {
                    container: None,
                    variable: "DB_URL".to_string(),
                }
            }
        );
    }

    #[test]
    fn deserialize_legacy_url_env_from() {
        let json = r#"{ "url": { "type": "env_from", "variable": "DB_URL" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::Url {
                url: TargetEnviromentVariableSource::EnvFrom {
                    container: None,
                    variable: "DB_URL".to_string(),
                }
            }
        );
    }

    #[test]
    fn deserialize_legacy_url_with_container() {
        let json = r#"{ "url": { "type": "env", "variable": "DB_URL", "container": "my-app" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::Url {
                url: TargetEnviromentVariableSource::Env {
                    container: Some("my-app".to_string()),
                    variable: "DB_URL".to_string(),
                }
            }
        );
    }

    #[test]
    fn deserialize_flat_url_env() {
        let json = r#"{ "type": "env", "url": "DB_URL" }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::FlatUrl {
                source_type: ConnectionSourceType::Env,
                url: "DB_URL".to_string(),
            }
        );
    }

    #[test]
    fn deserialize_flat_url_env_from() {
        let json = r#"{ "type": "env_from", "url": "DB_URL" }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::FlatUrl {
                source_type: ConnectionSourceType::EnvFrom,
                url: "DB_URL".to_string(),
            }
        );
    }

    #[test]
    fn deserialize_params_all() {
        let json = r#"{
            "type": "env",
            "params": {
                "host": "DB_HOST",
                "port": "DB_PORT",
                "user": "DB_USER",
                "password": "DB_PASSWORD",
                "database": "DB_NAME"
            }
        }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        match source {
            ConnectionSource::Params(config) => {
                assert_eq!(config.source_type, ConnectionSourceType::Env);
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_string()))
                );
                assert_eq!(
                    config.params.port,
                    Some(ParamSource::Variable("DB_PORT".to_string()))
                );
                assert_eq!(
                    config.params.user,
                    Some(ParamSource::Variable("DB_USER".to_string()))
                );
                assert_eq!(
                    config.params.password,
                    Some(ParamSource::Variable("DB_PASSWORD".to_string()))
                );
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_string()))
                );
            }
            other => panic!("expected Params, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_params_partial() {
        let json = r#"{ "type": "env", "params": { "host": "DB_HOST", "database": "DB_NAME" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        match source {
            ConnectionSource::Params(config) => {
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_string()))
                );
                assert!(config.params.port.is_none());
                assert!(config.params.user.is_none());
                assert!(config.params.password.is_none());
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_string()))
                );
            }
            other => panic!("expected Params, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_params_empty_accepts_defaults() {
        let json = r#"{ "type": "env", "params": {} }"#;
        let result = serde_json::from_str::<ConnectionSource>(json).unwrap();
        assert_eq!(
            result,
            ConnectionSource::Params(ConnectionParamsConfig {
                source_type: ConnectionSourceType::Env,
                params: ConnectionParamsVars {
                    host: None,
                    port: None,
                    user: None,
                    password: None,
                    database: None,
                },
            })
        );
    }

    #[test]
    fn deserialize_missing_url_and_params_fails() {
        let json = r#"{ "type": "env" }"#;
        let result = serde_json::from_str::<ConnectionSource>(json);
        assert!(result.is_err());
    }

    #[test]
    fn serialize_roundtrip_url() {
        let source = ConnectionSource::Url {
            url: TargetEnviromentVariableSource::Env {
                container: None,
                variable: "DB_URL".to_string(),
            },
        };
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn serialize_roundtrip_params() {
        let source = ConnectionSource::Params(ConnectionParamsConfig {
            source_type: ConnectionSourceType::Env,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("DB_HOST".to_string())),
                port: None,
                user: Some(ParamSource::Variable("DB_USER".to_string())),
                password: None,
                database: Some(ParamSource::Variable("DB_NAME".to_string())),
            },
        });
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn deserialize_params_with_secret_password() {
        let json = r#"{
            "type": "env",
            "params": {
                "host": "DB_HOST",
                "port": "DB_PORT",
                "user": "DB_USER",
                "password": { "secret": "rds-credentials", "key": "password" },
                "database": "DB_NAME"
            }
        }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        match source {
            ConnectionSource::Params(config) => {
                assert_eq!(config.source_type, ConnectionSourceType::Env);
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_string()))
                );
                assert_eq!(
                    config.params.password,
                    Some(ParamSource::Secret {
                        name: "rds-credentials".to_string(),
                        key: "password".to_string(),
                    })
                );
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_string()))
                );
            }
            other => panic!("expected Params, got {:?}", other),
        }
    }

    #[test]
    fn serialize_roundtrip_params_with_secret() {
        let source = ConnectionSource::Params(ConnectionParamsConfig {
            source_type: ConnectionSourceType::Env,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("DB_HOST".to_string())),
                port: None,
                user: None,
                password: Some(ParamSource::Secret {
                    name: "my-secret".to_string(),
                    key: "pass".to_string(),
                }),
                database: Some(ParamSource::Variable("DB_NAME".to_string())),
            },
        });
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn deserialize_param_source_invalid_object_fails() {
        let json = r#"{
            "type": "env",
            "params": {
                "host": { "invalid": "object" }
            }
        }"#;
        let result = serde_json::from_str::<ConnectionSource>(json);
        assert!(result.is_err());
    }
}
