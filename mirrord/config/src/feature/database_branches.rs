use std::{borrow::Cow, collections::BTreeMap, ops::Deref, path::PathBuf};

use mirrord_analytics::{Analytics, CollectAnalytics};
use mirrord_config_derive::MirrordConfig;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize, ser::SerializeMap};

use crate::{
    config::{self, ConfigError, source::MirrordConfigSource},
    feature::database_branches::redis::{LocalRedisBranchConfig, RemoteRedisBranchConfig},
};

/// Deserializes from either a single value or a JSON array.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleOrVec<T>(pub Vec<T>);

impl<T> SingleOrVec<T> {
    pub fn first(&self) -> Option<&T> {
        self.0.first()
    }
}

impl<T> From<T> for SingleOrVec<T> {
    fn from(value: T) -> Self {
        Self(vec![value])
    }
}

impl<T> From<Vec<T>> for SingleOrVec<T> {
    fn from(value: Vec<T>) -> Self {
        Self(value)
    }
}

impl<T> Deref for SingleOrVec<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for SingleOrVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a, T> IntoIterator for &'a SingleOrVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut SingleOrVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for SingleOrVec<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper<T> {
            Single(T),
            Multiple(Vec<T>),
        }

        match Helper::deserialize(deserializer)? {
            Helper::Single(v) => Ok(SingleOrVec::from(v)),
            Helper::Multiple(v) => Ok(SingleOrVec::from(v)),
        }
    }
}

impl<T: Serialize> Serialize for SingleOrVec<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0.as_slice() {
            [single] => single.serialize(serializer),
            many => many.serialize(serializer),
        }
    }
}

impl<T: JsonSchema> JsonSchema for SingleOrVec<T> {
    fn schema_name() -> Cow<'static, str> {
        Cow::Owned(format!("SingleOrVec_{}", T::schema_name()))
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let mut schema = schemars::json_schema!({});
        schema.insert(
            "x-kubernetes-preserve-unknown-fields".to_owned(),
            serde_json::Value::Bool(true),
        );
        schema
    }
}

pub mod clickhouse;
pub mod dynamodb;
pub mod generic;
pub mod mariadb;
pub mod mongodb;
pub mod mssql;
pub mod mysql;
pub mod pg;
pub mod redis;
pub mod spanner;

pub use clickhouse::{
    ClickhouseBranchConfig, ClickhouseBranchCopyConfig, ClickhouseBranchTableCopyConfig,
};
pub use dynamodb::{
    DynamodbBranchCollectionCopyConfig, DynamodbBranchConfig, DynamodbBranchCopyConfig,
};
pub use generic::{GenericBranchConfig, GenericReadinessConfig};
pub use mariadb::{MariadbBranchConfig, MariadbBranchCopyConfig, MariadbBranchTableCopyConfig};
pub use mongodb::{
    MongodbBranchCollectionCopyConfig, MongodbBranchConfig, MongodbBranchCopyConfig,
};
pub use mssql::{MssqlBranchConfig, MssqlBranchCopyConfig, MssqlBranchTableCopyConfig};
pub use mysql::{MysqlBranchConfig, MysqlBranchCopyConfig, MysqlBranchTableCopyConfig};
pub use pg::{PgBranchConfig, PgBranchCopyConfig, PgBranchTableCopyConfig};
pub use redis::{
    RedisBranchConfig, RedisBranchCopyConfig, RedisConnectionConfig, RedisLocalConfig,
    RedisOptions, RedisRuntime, RedisValueSource,
};
pub use spanner::{SpannerBranchConfig, SpannerBranchCopyConfig, SpannerBranchTableCopyConfig};

pub type PgIamAuthConfig = IamAuthConfig;

/// Shared copy config for individual items (tables, collections, etc.).
/// All database engines use this same struct for per-item copy configuration.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchItemCopyConfig {
    pub filter: Option<String>,
}

/// <!--${internal}-->
/// Runs schema migrations on a SQL branch. Documented on [`DatabaseBranchConfig`].
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "flavor", rename_all = "lowercase", deny_unknown_fields)]
pub enum SqlBranchMigrationsConfig {
    /// Apply migrations with [Flyway](https://documentation.red-gate.com/flyway).
    Flyway {
        /// Local directory holding the migration files.
        ///
        /// Resolved relative to the working directory.
        path: PathBuf,
        /// Container image override for the migration runner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
}

impl SqlBranchMigrationsConfig {
    fn verify(&self, base: &DatabaseBranchBaseConfig) -> Result<(), ConfigError> {
        if base.name.is_some() {
            Ok(())
        } else {
            const MESSAGE: &str = "`feature.db_branches[].migrations` requires `feature.db_branches[].name` to be set.";

            Err(ConfigError::Conflict(MESSAGE.to_owned()))
        }
    }
}

/// IAM authentication for the source database.
/// Use this when your source database (AWS RDS, GCP Cloud SQL) requires IAM authentication
/// instead of password-based authentication.
///
/// Environment variable sources follow the same pattern as `connection.url`:
/// - `{ "type": "env", "variable": "VAR_NAME" }` - direct env var from pod spec
/// - `{ "type": "env_from", "variable": "VAR_NAME" }` - from configMapRef/secretRef
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IamAuthConfig {
    /// For AWS RDS/Aurora IAM authentication, set `type` to `"aws_rds"`.
    ///
    /// Credentials for signing the RDS auth token come from one of two setups:
    /// - Static keys: the operator copies `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and
    ///   optionally `AWS_SESSION_TOKEN` from the target pod's environment (or from the env vars
    ///   named in the fields below) to the branch pod.
    /// - IRSA / EKS Pod Identity: when the target pod has no static keys, the branch pod runs under
    ///   the target's service account and receives the same IAM role. No key fields are needed; `{
    ///   "type": "aws_rds" }` is enough.
    ///
    /// Example with explicit env var sources:
    /// ```json
    /// {
    ///   "iam_auth": {
    ///     "type": "aws_rds",
    ///     "region": { "type": "env", "variable": "MY_AWS_REGION" },
    ///     "access_key_id": { "type": "env_from", "variable": "AWS_KEY" }
    ///   }
    /// }
    /// ```
    ///
    /// Parameters:
    /// - `region`: AWS region. If not specified, uses AWS_REGION or AWS_DEFAULT_REGION from the
    ///   target pod. With IRSA, set it explicitly if neither var is in the target's pod spec.
    /// - `access_key_id`: AWS Access Key ID. If not specified, uses AWS_ACCESS_KEY_ID.
    /// - `secret_access_key`: AWS Secret Access Key. If not specified, uses AWS_SECRET_ACCESS_KEY.
    /// - `session_token`:  AWS Session Token (for temporary credentials). If not specified, uses
    ///   AWS_SESSION_TOKEN.
    AwsRds {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<TargetEnvironmentVariableSource>,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        access_key_id: Option<TargetEnvironmentVariableSource>,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_access_key: Option<TargetEnvironmentVariableSource>,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_token: Option<TargetEnvironmentVariableSource>,
    },
    /// For GCP Cloud SQL IAM authentication, set `type` to `"gcp_cloud_sql"`.
    ///
    /// Example for GCP Cloud SQL with credentials from a secret:
    /// ```json
    /// {
    ///   "iam_auth": {
    ///     "type": "gcp_cloud_sql",
    ///     "credentials_json": { "type": "env_from", "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON" }
    ///   }
    /// }
    /// ```
    ///
    /// The init container must have GCP credentials (via Workload Identity or service account key).
    /// Use either `credentials_json` OR `credentials_path`, not both.
    ///
    /// Parameters:
    /// - `credentials_json`: Inline service account JSON key content. Specify the env var that
    ///   contains the raw JSON content of the service account key. Example: ` { "type": "env",
    ///   "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON" } `.
    /// - `credentials_path`: Path to service account JSON key file. Specify the env var that
    ///   contains the file path to the service account key. The file must be accessible from the
    ///   init container. Example: `{"type": "env", "variable": "GOOGLE_APPLICATION_CREDENTIALS"}`.
    /// - `project`: GCP project ID. If not specified, uses GOOGLE_CLOUD_PROJECT or GCP_PROJECT.
    GcpCloudSql {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_json: Option<TargetEnvironmentVariableSource>,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_path: Option<TargetEnvironmentVariableSource>,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<TargetEnvironmentVariableSource>,
    },
}

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
    pub fn count_clickhouse(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Clickhouse { .. }))
            .count()
    }

    pub fn count_dynamodb(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Dynamodb { .. }))
            .count()
    }

    pub fn count_generic(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Generic { .. }))
            .count()
    }

    pub fn count_mariadb(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Mariadb { .. }))
            .count()
    }

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

    pub fn count_mssql(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Mssql { .. }))
            .count()
    }

    pub fn count_redis(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Redis { .. }))
            .count()
    }

    pub fn count_spanner(&self) -> usize {
        self.0
            .iter()
            .filter(|db| matches!(db, DatabaseBranchConfig::Spanner { .. }))
            .count()
    }

    /// Verifies invariants that span individual branch configs (e.g. `ttl_secs`/`ttl_mins`
    /// mutual exclusion).
    pub fn verify(&self, context: &mut config::ConfigContext) -> Result<(), ConfigError> {
        for branch in &self.0 {
            match branch {
                DatabaseBranchConfig::Clickhouse(cfg) => cfg.base.verify()?,
                DatabaseBranchConfig::Dynamodb(cfg) => cfg.base.verify()?,
                DatabaseBranchConfig::Generic(cfg) => cfg.verify(context)?,
                DatabaseBranchConfig::Mariadb(cfg) => {
                    cfg.base.verify()?;

                    cfg.migrations
                        .as_ref()
                        .map(|migrations| migrations.verify(&cfg.base))
                        .transpose()?;
                }
                DatabaseBranchConfig::Mongodb(cfg) => cfg.base.verify()?,
                DatabaseBranchConfig::Mssql(cfg) => {
                    cfg.base.verify()?;

                    cfg.migrations
                        .as_ref()
                        .map(|migrations| migrations.verify(&cfg.base))
                        .transpose()?;
                }
                DatabaseBranchConfig::Mysql(cfg) => {
                    cfg.base.verify()?;

                    cfg.migrations
                        .as_ref()
                        .map(|migrations| migrations.verify(&cfg.base))
                        .transpose()?;
                }
                DatabaseBranchConfig::Pg(cfg) => {
                    cfg.base.verify()?;

                    cfg.migrations
                        .as_ref()
                        .map(|migrations| migrations.verify(&cfg.base))
                        .transpose()?;
                }
                DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                    RedisBranchConfig::Local(_) => continue,
                    RedisBranchConfig::Remote(remote) => remote.verify()?,
                },
                DatabaseBranchConfig::Spanner(cfg) => cfg.base.verify()?,
            }
        }
        Ok(())
    }
}

impl DatabaseBranchConfig {
    /// The shared base config of this branch, when the variant has one (local Redis
    /// branches don't - they never reach the operator).
    pub fn base(&self) -> Option<&DatabaseBranchBaseConfig> {
        match self {
            DatabaseBranchConfig::Clickhouse(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Dynamodb(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Generic(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Mariadb(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Mongodb(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Mssql(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Mysql(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Pg(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                RedisBranchConfig::Local(_) => None,
                RedisBranchConfig::Remote(remote) => Some(&remote.base),
            },
            DatabaseBranchConfig::Spanner(cfg) => Some(&cfg.base),
        }
    }

    /// Names of target-pod env vars that the operator uses to redirect this branch's
    /// connection. Locally overriding any of these (via `feature.env.override`) would
    /// fight the operator's redirection, so [`LayerConfig::verify`] rejects such configs.
    ///
    /// [`LayerConfig::verify`]: crate::LayerConfig::verify
    pub(crate) fn connection_env_keys(&self) -> Vec<&str> {
        let mut keys = Vec::new();

        match self {
            DatabaseBranchConfig::Clickhouse(cfg) => {
                cfg.base.connection.collect_env_keys(&mut keys)
            }
            DatabaseBranchConfig::Dynamodb(cfg) => cfg.base.connection.collect_env_keys(&mut keys),
            // The operator redirects only the host/port vars of a generic branch; the app's
            // other vars (user/password/database/extras) are deliberately left untouched.
            DatabaseBranchConfig::Generic(cfg) => cfg.collect_redirected_env_keys(&mut keys),
            DatabaseBranchConfig::Mariadb(cfg) => cfg.base.connection.collect_env_keys(&mut keys),
            DatabaseBranchConfig::Mongodb(cfg) => cfg.base.connection.collect_env_keys(&mut keys),
            DatabaseBranchConfig::Mssql(cfg) => cfg.base.connection.collect_env_keys(&mut keys),
            DatabaseBranchConfig::Mysql(cfg) => cfg.base.connection.collect_env_keys(&mut keys),
            DatabaseBranchConfig::Pg(cfg) => cfg.base.connection.collect_env_keys(&mut keys),
            DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                RedisBranchConfig::Local(LocalRedisBranchConfig { connection, .. }) => {
                    connection.collect_env_keys(&mut keys)
                }
                RedisBranchConfig::Remote(RemoteRedisBranchConfig { base, .. }) => {
                    base.connection.collect_env_keys(&mut keys)
                }
            },
            // Spanner leaves the app's project/instance/database vars untouched; the operator
            // only injects the emulator host var, so that is the sole redirected key.
            DatabaseBranchConfig::Spanner(cfg) => keys.push(cfg.emulator_host.as_str()),
        };

        keys
    }
}

impl ConnectionSource {
    fn collect_env_keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Url { url } => url.collect_env_keys(out),
            Self::FlatUrl { url, .. } => out.extend(url.iter().map(String::as_str)),
            Self::Params(config) => config.params.collect_env_keys(out),
        }
    }
}

impl TargetEnvironmentVariableSource {
    fn collect_env_keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Env { variable, .. } | Self::EnvFrom { variable, .. } => out.push(variable),
            Self::Secret {
                env_var_name: Some(name),
                ..
            }
            | Self::GcpSecretManager {
                env_var_name: Some(name),
                ..
            } => out.push(name),
            Self::Secret {
                env_var_name: None, ..
            }
            | Self::GcpSecretManager {
                env_var_name: None, ..
            } => {}
        }
    }
}

impl ConnectionParamsVars {
    fn collect_env_keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        [
            &self.host,
            &self.port,
            &self.user,
            &self.password,
            &self.database,
        ]
        .iter()
        .filter_map(|t| t.as_ref())
        .flatten()
        .for_each(|var| var.collect_env_keys(out));
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
///
/// The fields below are shared by every engine. Engine-specific fields (copy modes,
/// `iam_auth`, `connection_settings`, `emulator_host`) are documented under each `type`.
///
/// #### feature.db_branches[].id (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-id}
///
/// Users can choose to specify a unique `id`. This is useful for reusing or sharing
/// the same database branch among Kubernetes users.
///
/// #### feature.db_branches[].name (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-name}
///
/// When source database connection detail is not accessible to mirrord operator, users
/// can specify the database `name` so it is included in the connection options mirrord
/// uses as the override.
///
/// #### feature.db_branches[].ttl_secs (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-ttl_secs}
///
/// Mirrord operator starts counting the TTL when a branch is no longer used by any session.
/// The time-to-live (TTL) for the branch database is set to 300 seconds by default.
/// Users can set `ttl_secs` to customize this value according to their need. Please note
/// that longer TTL paired with frequent mirrord session turnover can result in increased
/// resource usage. For this reason, branch database TTL caps out at 15 min.
///
/// Mutually exclusive with [`ttl_mins`](#feature-db_branches-sql-ttl_mins).
///
/// #### feature.db_branches[].ttl_mins (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-ttl_mins}
///
/// Same as [`ttl_secs`](#feature-db_branches-sql-ttl_secs) but expressed in minutes.
///
/// Mutually exclusive with [`ttl_secs`](#feature-db_branches-sql-ttl_secs).
///
/// #### feature.db_branches[].creation_timeout_secs (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-creation_timeout_secs}
///
/// The timeout in seconds to wait for a database branch to become ready after creation.
/// Defaults to 60 seconds. Adjust this value based on your database size and cluster
/// performance.
///
/// #### feature.db_branches[].version (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-version}
///
/// Mirrord operator uses a default version of the database image unless `version` is given.
///
/// Mutually exclusive with [`image`](#feature-db_branches-sql-image).
///
/// #### feature.db_branches[].image (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-image}
///
/// Full image reference for the branch database container, including the tag
/// (e.g. `registry.example.com/postgresql:15-partman`). Setting `image` overrides both the
/// operator's built-in default image and any registry configured cluster-wide by the operator
/// admin. Cluster admins can restrict which images are accepted with the per-database
/// `dbPod.allowedImages` list in the operator's Helm values; when that list is not set, any
/// image is allowed.
///
/// Mutually exclusive with [`version`](#feature-db_branches-sql-version), as the image
/// reference already carries the tag.
///
/// #### feature.db_branches[].connection (type: mysql, mariadb, pg, mongodb, mssql, redis) {#feature-db_branches-sql-connection}
///
/// `connection` describes how to get the connection information to the source database.
/// When the branch database is ready for use, Mirrord operator will replace the connection
/// information with the branch database's. It accepts a connection URL or individual params:
///
/// ```json
/// { "url": { "type": "env", "variable": "DB_CONNECTION_URL" } }
/// ```
/// ```json
/// { "type": "env", "url": "DB_CONNECTION_URL" }
/// ```
/// ```json
/// { "type": "env", "params": { "host": "DB_HOST", "port": "DB_PORT", "user": "DB_USER", "password": "DB_PASSWORD", "database": "DB_NAME" } }
/// ```
///
/// Any param can also be read from a Kubernetes Secret instead of a target-pod env var:
///
/// ```json
/// { "type": "env", "params": { "host": "DB_HOST", "password": { "secret": "my-secret", "key": "password" }, "database": "DB_NAME" } }
/// ```
///
/// #### feature.db_branches[].migrations (type: mysql, mariadb, pg, mssql, clickhouse) {#feature-db_branches-sql-migrations}
///
/// Schema migrations to run on the branch after it is created. Currently supports
/// [Flyway](https://documentation.red-gate.com/flyway):
///
/// ```json
/// { "migrations": { "flavor": "flyway", "path": "./migrations" } }
/// ```
///
/// - `path`: local directory holding the migration files, resolved relative to the working
///   directory.
/// - `image`: optional container image override for the migration runner.
///
/// Requires [`name`](#feature-db_branches-sql-name) to be set.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum DatabaseBranchConfig {
    Clickhouse(Box<ClickhouseBranchConfig>),
    Dynamodb(Box<DynamodbBranchConfig>),
    Generic(Box<GenericBranchConfig>),
    Mariadb(Box<MariadbBranchConfig>),
    Mongodb(Box<MongodbBranchConfig>),
    Mssql(Box<MssqlBranchConfig>),
    Mysql(Box<MysqlBranchConfig>),
    Pg(Box<PgBranchConfig>),
    Redis(Box<RedisBranchConfig>),
    Spanner(Box<SpannerBranchConfig>),
}

/// <!--${internal}-->
/// Fields shared by every database branch config. They are documented once on
/// [`DatabaseBranchConfig`] so the generated config docs do not repeat them for each engine;
/// keep only short schema descriptions here.
#[derive(MirrordConfig, Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[config(map_to = "DatabaseBranchBaseFileConfig")]
pub struct DatabaseBranchBaseConfig {
    /// Optional stable id for reusing or sharing a branch across users.
    pub id: Option<String>,

    /// Source database name, used when the operator cannot read it from the connection.
    pub name: Option<String>,

    /// Branch TTL in seconds, counted from when the branch is last used. Mutually exclusive
    /// with `ttl_mins`. Defaults to 300, capped at 15 minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,

    /// Branch TTL in minutes, counted from when the branch is last used. Mutually exclusive
    /// with `ttl_secs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_mins: Option<u64>,

    /// Seconds to wait for a branch to become ready after creation. Defaults to 60.
    #[serde(default = "default_creation_timeout_secs")]
    pub creation_timeout_secs: u64,

    /// Source database image version. Defaults to the operator's built-in version.
    pub version: Option<String>,

    /// Full image reference for the branch container, including the tag. Overrides the
    /// operator-configured registry entirely. Mutually exclusive with `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// How to source the connection info for the source database. The operator swaps it for
    /// the branch's connection once the branch is ready.
    pub connection: ConnectionSource,
}

impl DatabaseBranchBaseConfig {
    /// Default TTL in seconds applied when neither `ttl_secs` nor `ttl_mins` is set.
    pub const DEFAULT_TTL_SECS: u64 = 300;

    /// Returns the configured TTL in seconds. `ttl_mins` (if set) takes precedence over
    /// `ttl_secs`; if both are unset, [`Self::DEFAULT_TTL_SECS`] is returned. Configurations
    /// that set both fields are rejected by [`Self::verify`].
    pub fn resolved_ttl_secs(&self) -> u64 {
        if let Some(mins) = self.ttl_mins {
            mins.saturating_mul(60)
        } else {
            self.ttl_secs.unwrap_or(Self::DEFAULT_TTL_SECS)
        }
    }

    pub fn verify(&self) -> Result<(), ConfigError> {
        if self.ttl_secs.is_some() && self.ttl_mins.is_some() {
            return Err(ConfigError::Conflict(
                "`feature.db_branches[].ttl_secs` and `feature.db_branches[].ttl_mins` \
                 cannot both be set."
                    .to_owned(),
            ));
        }
        if self.image.is_some() && self.version.is_some() {
            return Err(ConfigError::Conflict(
                "`feature.db_branches[].image` and `feature.db_branches[].version` cannot \
                 both be set; the image reference includes the tag."
                    .to_owned(),
            ));
        }
        Ok(())
    }
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
#[serde(untagged, deny_unknown_fields)]
pub enum ConnectionSource {
    Url {
        url: TargetEnvironmentVariableSource,
    },
    FlatUrl {
        #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
        source_type: Option<ConnectionSourceType>,
        url: SingleOrVec<String>,
    },
    Params(Box<ConnectionParamsConfig>),
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
                let entries = if source_type.is_some() { 2 } else { 1 };
                let mut map = serializer.serialize_map(Some(entries))?;
                if let Some(st) = source_type {
                    map.serialize_entry("type", st)?;
                }
                map.serialize_entry("url", url)?;
                map.end()
            }
            Self::Params(config) => {
                let entries = if config.source_type.is_some() { 2 } else { 1 };
                let mut map = serializer.serialize_map(Some(entries))?;
                if let Some(ref st) = config.source_type {
                    map.serialize_entry("type", st)?;
                }
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
/// The `type` field is optional - when omitted, the operator auto-detects
/// whether the variable comes from `env` or `envFrom` on the target pod.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionParamsConfig {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<ConnectionSourceType>,
    pub params: ConnectionParamsVars,
}

/// <!--${internal}-->
/// A connection parameter source: a plain env var name (string), an env var with a literal
/// value override (object with `variable` and optional `value`), or a Kubernetes Secret
/// reference.
///
/// As a string: `"DB_HOST"` - resolved using the parent `type` field (env or env_from).
///
/// As an object with a literal value: `{ "variable": "DB_HOST", "value": "myhost.com" }` -
/// uses the provided `value` directly instead of reading the env var from the target pod.
/// The `variable` names the key in the credential Secret that the CLI creates.
///
/// As a value-only object: `{ "value": "myhost.com" }` - provides the value directly without
/// referencing any env var on the target pod.
///
/// As a Secret ref: `{ "secret": "my-secret", "key": "password" }` - read directly from a
/// Kubernetes Secret. Add `"env_var_name": "DB_PASSWORD"` to also expose the resolved
/// value to the local process under that name. Without `env_var_name` the Secret is
/// only consumed by the operator for branch provisioning; the local app must get the
/// credential from the target pod's environment.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ParamSource {
    Variable(String),
    Secret {
        #[serde(rename = "secret")]
        name: String,
        key: String,
        /// Name of the env var to set on the local process from the resolved
        /// Secret value. When `None`, the operator only uses the Secret for
        /// branch provisioning and does not inject anything locally.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },
    Pattern {
        env_var_name: String,
        value_pattern: String,
    },
    Env {
        #[serde(alias = "variable")]
        env_var_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    /// Value fetched from Google Secret Manager at branch data-copy time by the
    /// init container, using the target pod's service account (Workload Identity).
    /// `gcp_secret_manager` is a GSM resource name, e.g.
    /// `projects/my-project/secrets/db-password/versions/latest`. mirrord does not
    /// read the value; only the branch init container does, so the operator needs
    /// no access to the secret.
    ///
    /// Setup: the branch pod inherits the target pod's service account, so that
    /// account's Google identity (via GKE Workload Identity) must have
    /// `roles/secretmanager.secretAccessor` on the secret. No operator-level
    /// permissions are required.
    ///
    /// Add `env_var_name` to also point the local app at the branch DB under that
    /// name (same semantics as `Secret`). Without it the value is only used to
    /// provision the branch and the local app keeps reading its own source.
    GcpSecretManager {
        #[serde(rename = "gcp_secret_manager")]
        secret_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },
}

impl ParamSource {
    pub fn as_variable(&self) -> Option<&str> {
        match self {
            Self::Variable(v) => Some(v),
            Self::Env { env_var_name, .. } | Self::Pattern { env_var_name, .. } => {
                Some(env_var_name)
            }
            Self::Secret { .. } | Self::GcpSecretManager { .. } => None,
        }
    }

    fn collect_env_keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            ParamSource::Variable(v) => out.push(v),
            ParamSource::Env { env_var_name, .. } | ParamSource::Pattern { env_var_name, .. } => {
                out.push(env_var_name)
            }
            ParamSource::Secret {
                env_var_name: Some(name),
                ..
            }
            | ParamSource::GcpSecretManager {
                env_var_name: Some(name),
                ..
            } => out.push(name),
            ParamSource::Secret {
                env_var_name: None, ..
            }
            | ParamSource::GcpSecretManager {
                env_var_name: None, ..
            } => {}
        }
    }
}

/// Individual database connection parameter sources.
/// At least one parameter must be specified.
/// Each parameter is either a plain string (env var name) or an object with `secret` and `key`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct ConnectionParamsVars {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<SingleOrVec<ParamSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<SingleOrVec<ParamSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<SingleOrVec<ParamSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SingleOrVec<ParamSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<SingleOrVec<ParamSource>>,

    /// Engine-specific connection parameters that have no universal slot above, keyed by a name
    /// the engine recognizes. They are written flat alongside the fixed slots, so a Spanner
    /// `params` block reads `{ "project": ..., "instance": ..., "database_id": ... }` with no
    /// nesting. Unlike the fixed slots, these are read-only source locators: the operator resolves
    /// each from the target pod and hands it to the branch init sidecar, and never overrides it on
    /// the local app.
    ///
    /// Google Cloud Spanner is the only engine that uses this. Each key names the env var on the
    /// target pod that holds one of Spanner's three separate source identifiers:
    /// - `project`: the GCP project id the source Spanner instance lives in.
    /// - `instance`: the source Spanner instance id within that project.
    /// - `database_id`: the source database id to recreate in the emulator (and, for the `schema`
    ///   / `all` copy modes, copy schema and data from).
    ///
    /// Spanner uses `database_id` rather than the fixed `database` slot above because the two mean
    /// different things. The fixed slot is an override target: the operator rewrites the app's
    /// database var to point at the branch's database. Spanner never rewrites it - the app keeps
    /// its own database id and is redirected wholesale by `SPANNER_EMULATOR_HOST` - so its
    /// database is a read-only locator the init sidecar uses to pick which source database to
    /// recreate, exactly like `project` and `instance`. The distinct name also keeps it from
    /// colliding with the flattened fixed `database` slot.
    ///
    /// Unknown keys are rejected by the operator for the resolved engine.
    #[serde(flatten)]
    pub extra: BTreeMap<String, SingleOrVec<ParamSource>>,
}

/// <!--${internal}-->
/// Different ways to source the connection options.
///
/// Support:
/// - `env` in the target's pod spec.
/// - `envFrom` in the target's pod spec.
/// - `secret` read directly from a Kubernetes Secret.
/// - `gcp_secret_manager` fetched from Google Secret Manager by the init container.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[schemars(rename = "DbBranchingConnectionSourceKind")]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetEnvironmentVariableSource {
    Env {
        container: Option<String>,
        variable: String,
        /// Literal value for this connection parameter. The CLI sends it to the
        /// operator, which stores it in a Kubernetes Secret for the branch pod.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    EnvFrom {
        container: Option<String>,
        variable: String,
    },
    Secret {
        name: String,
        key: String,
        /// Name of the env var to set on the local process from the resolved
        /// Secret value. Same semantics as on `ParamSource::Secret`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },
    /// Fetched from Google Secret Manager by the branch init container using the
    /// target pod's service account. Same semantics as `ParamSource::GcpSecretManager`.
    GcpSecretManager {
        secret_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
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
        analytics.add("clickhouse_branch_count", self.count_clickhouse());
        analytics.add("generic_branch_count", self.count_generic());
        analytics.add("mariadb_branch_count", self.count_mariadb());
        analytics.add("mongodb_branch_count", self.count_mongodb());
        analytics.add("mssql_branch_count", self.count_mssql());
        analytics.add("mysql_branch_count", self.count_mysql());
        analytics.add("pg_branch_count", self.count_pg());
        analytics.add("redis_branch_count", self.count_redis());
        analytics.add("spanner_branch_count", self.count_spanner());
    }
}

pub fn default_creation_timeout_secs() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn deserialize_legacy_url_env() {
        let json = r#"{ "url": { "type": "env", "variable": "DB_URL" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::Url {
                url: TargetEnvironmentVariableSource::Env {
                    container: None,
                    variable: "DB_URL".to_owned(),
                    value: None,
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
                url: TargetEnvironmentVariableSource::EnvFrom {
                    container: None,
                    variable: "DB_URL".to_owned(),
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
                url: TargetEnvironmentVariableSource::Env {
                    container: Some("my-app".to_owned()),
                    variable: "DB_URL".to_owned(),
                    value: None,
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
                source_type: Some(ConnectionSourceType::Env),
                url: "DB_URL".to_owned().into(),
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
                source_type: Some(ConnectionSourceType::EnvFrom),
                url: "DB_URL".to_owned().into(),
            }
        );
    }

    #[test]
    fn deserialize_flat_url_no_type() {
        let json = r#"{ "url": "DB_URL" }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        assert_eq!(
            source,
            ConnectionSource::FlatUrl {
                source_type: None,
                url: "DB_URL".to_owned().into(),
            }
        );
    }

    #[test]
    fn deserialize_params_all() {
        let json = r#"{
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
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_owned()).into())
                );
                assert_eq!(
                    config.params.port,
                    Some(ParamSource::Variable("DB_PORT".to_owned()).into())
                );
                assert_eq!(
                    config.params.user,
                    Some(ParamSource::Variable("DB_USER".to_owned()).into())
                );
                assert_eq!(
                    config.params.password,
                    Some(ParamSource::Variable("DB_PASSWORD".to_owned()).into())
                );
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_owned()).into())
                );
            }
            other => panic!("expected Params, got {:?}", other),
        }
    }

    /// Engine-specific keys (Spanner's `project`/`instance`/`database_id`) are written flat next to
    /// the fixed slots and land in `extra`, while a `database` key still binds to the fixed slot.
    #[test]
    fn deserialize_params_extra_flattened() {
        let json = r#"{
            "params": {
                "database": "DB_NAME",
                "project": "GOOGLE_CLOUD_PROJECT",
                "instance": "SPANNER_INSTANCE_ID",
                "database_id": "SPANNER_DATABASE_ID"
            }
        }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        let ConnectionSource::Params(config) = source else {
            panic!("expected Params");
        };
        // `database` binds to the fixed slot, not into `extra`.
        assert_eq!(
            config.params.database,
            Some(ParamSource::Variable("DB_NAME".to_owned()).into())
        );
        assert!(!config.params.extra.contains_key("database"));
        // The three Spanner locators land in `extra`, keyed by their flat names.
        let expected: BTreeMap<String, SingleOrVec<ParamSource>> = BTreeMap::from([
            (
                "project".to_owned(),
                ParamSource::Variable("GOOGLE_CLOUD_PROJECT".to_owned()).into(),
            ),
            (
                "instance".to_owned(),
                ParamSource::Variable("SPANNER_INSTANCE_ID".to_owned()).into(),
            ),
            (
                "database_id".to_owned(),
                ParamSource::Variable("SPANNER_DATABASE_ID".to_owned()).into(),
            ),
        ]);
        assert_eq!(config.params.extra, expected);
    }

    /// A Spanner branch parses with its source locators flat under `connection.params` and the
    /// emulator-host var defaulting to `SPANNER_EMULATOR_HOST`.
    #[test]
    fn deserialize_spanner_branch_flat_params() {
        let json = r#"{
            "type": "spanner",
            "connection": {
                "params": {
                    "project": "GOOGLE_CLOUD_PROJECT",
                    "instance": "SPANNER_INSTANCE_ID",
                    "database_id": "SPANNER_DATABASE_ID"
                }
            }
        }"#;
        let branch: DatabaseBranchConfig = serde_json::from_str(json).unwrap();
        let DatabaseBranchConfig::Spanner(spanner) = branch else {
            panic!("expected Spanner branch");
        };
        assert_eq!(spanner.emulator_host, "SPANNER_EMULATOR_HOST");
        let ConnectionSource::Params(config) = &spanner.base.connection else {
            panic!("expected Params connection");
        };
        assert!(config.params.host.is_none());
        assert_eq!(
            config.params.extra.get("database_id"),
            Some(&ParamSource::Variable("SPANNER_DATABASE_ID".to_owned()).into())
        );
    }

    #[test]
    fn deserialize_params_partial() {
        let json = r#"{ "params": { "host": "DB_HOST", "database": "DB_NAME" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        match source {
            ConnectionSource::Params(config) => {
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_owned()).into())
                );
                assert!(config.params.port.is_none());
                assert!(config.params.user.is_none());
                assert!(config.params.password.is_none());
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_owned()).into())
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
            ConnectionSource::Params(Box::new(ConnectionParamsConfig {
                source_type: Some(ConnectionSourceType::Env),
                params: ConnectionParamsVars {
                    host: None,
                    port: None,
                    user: None,
                    password: None,
                    database: None,
                    extra: Default::default(),
                },
            }))
        );
    }

    #[test]
    fn deserialize_params_no_type() {
        let json = r#"{ "params": { "host": "DB_HOST", "database": "DB_NAME" } }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        match source {
            ConnectionSource::Params(config) => {
                assert_eq!(config.source_type, None);
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_owned()).into())
                );
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_owned()).into())
                );
            }
            other => panic!("expected Params, got {:?}", other),
        }
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
            url: TargetEnvironmentVariableSource::Env {
                container: None,
                variable: "DB_URL".to_owned(),
                value: None,
            },
        };
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn serialize_roundtrip_url_gcp_secret_manager() {
        let source = ConnectionSource::Url {
            url: TargetEnvironmentVariableSource::GcpSecretManager {
                secret_ref: "projects/p/secrets/db-url/versions/latest".to_owned(),
                env_var_name: Some("DATABASE_URL".to_owned()),
            },
        };
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized, "json was: {json}");
    }

    #[test]
    fn serialize_roundtrip_params() {
        let source = ConnectionSource::Params(Box::new(ConnectionParamsConfig {
            source_type: None,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("DB_HOST".to_owned()).into()),
                port: None,
                user: Some(ParamSource::Variable("DB_USER".to_owned()).into()),
                password: None,
                database: Some(ParamSource::Variable("DB_NAME".to_owned()).into()),
                extra: Default::default(),
            },
        }));
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn mysql_copy_dump_args_parse_for_all_modes() {
        let empty: MysqlBranchCopyConfig = serde_json::from_str(
            r#"{
                "mode": "empty",
                "tables": {
                    "users": { "filter": "active = true" }
                },
                "dump_args": ["--single-transaction"]
            }"#,
        )
        .unwrap();
        assert_eq!(
            empty,
            MysqlBranchCopyConfig::Empty {
                tables: Some(BTreeMap::from([(
                    "users".to_owned(),
                    BranchItemCopyConfig {
                        filter: Some("active = true".to_owned())
                    }
                )])),
                dump_args: Some(vec!["--single-transaction".to_owned()])
            }
        );

        let schema: MysqlBranchCopyConfig =
            serde_json::from_str(r#"{ "mode": "schema", "dump_args": [] }"#).unwrap();
        assert_eq!(
            schema,
            MysqlBranchCopyConfig::Schema {
                tables: None,
                dump_args: Some(vec![])
            }
        );

        let all: MysqlBranchCopyConfig =
            serde_json::from_str(r#"{ "mode": "all", "dump_args": ["--no-tablespaces"] }"#)
                .unwrap();
        assert_eq!(
            all,
            MysqlBranchCopyConfig::All {
                dump_args: Some(vec!["--no-tablespaces".to_owned()])
            }
        );
    }

    #[test]
    fn pg_copy_dump_args_parse_for_all_modes() {
        let empty: PgBranchCopyConfig = serde_json::from_str(
            r#"{
                "mode": "empty",
                "tables": {
                    "users": { "filter": "active = true" }
                },
                "dump_args": ["--no-owner"]
            }"#,
        )
        .unwrap();
        assert_eq!(
            empty,
            PgBranchCopyConfig::Empty {
                tables: Some(BTreeMap::from([(
                    "users".to_owned(),
                    BranchItemCopyConfig {
                        filter: Some("active = true".to_owned())
                    }
                )])),
                dump_args: Some(vec!["--no-owner".to_owned()])
            }
        );

        let schema: PgBranchCopyConfig =
            serde_json::from_str(r#"{ "mode": "schema", "dump_args": [] }"#).unwrap();
        assert_eq!(
            schema,
            PgBranchCopyConfig::Schema {
                tables: None,
                dump_args: Some(vec![])
            }
        );

        let all: PgBranchCopyConfig =
            serde_json::from_str(r#"{ "mode": "all", "dump_args": ["--no-acl"] }"#).unwrap();
        assert_eq!(
            all,
            PgBranchCopyConfig::All {
                dump_args: Some(vec!["--no-acl".to_owned()])
            }
        );
    }

    #[test]
    fn deserialize_params_with_secret_password() {
        let json = r#"{
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
                assert_eq!(
                    config.params.host,
                    Some(ParamSource::Variable("DB_HOST".to_owned()).into())
                );
                assert_eq!(
                    config.params.password,
                    Some(
                        ParamSource::Secret {
                            name: "rds-credentials".to_owned(),
                            key: "password".to_owned(),
                            env_var_name: None,
                        }
                        .into()
                    )
                );
                assert_eq!(
                    config.params.database,
                    Some(ParamSource::Variable("DB_NAME".to_owned()).into())
                );
            }
            other => panic!("expected Params, got {:?}", other),
        }
    }

    #[test]
    fn serialize_roundtrip_params_with_secret() {
        let source = ConnectionSource::Params(Box::new(ConnectionParamsConfig {
            source_type: None,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("DB_HOST".to_owned()).into()),
                port: None,
                user: None,
                password: Some(
                    ParamSource::Secret {
                        name: "my-secret".to_owned(),
                        key: "pass".to_owned(),
                        env_var_name: None,
                    }
                    .into(),
                ),
                database: Some(ParamSource::Variable("DB_NAME".to_owned()).into()),
                extra: Default::default(),
            },
        }));
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized);
    }

    #[test]
    fn deserialize_param_source_invalid_object_fails() {
        let json = r#"{
            "params": {
                "host": { "invalid": "object" }
            }
        }"#;
        let result = serde_json::from_str::<ConnectionSource>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_pattern_and_multi_source() {
        let json = r#"{
            "params": {
                "host": { "env_var_name": "MYSQL_SERVER", "value_pattern": "^([^:]+):" },
                "port": { "env_var_name": "MYSQL_SERVER", "value_pattern": ":([0-9]+)$" },
                "user": "DB_USER",
                "password": { "secret": "rds-creds", "key": "password" }
            }
        }"#;
        let source: ConnectionSource = serde_json::from_str(json).unwrap();
        match &source {
            ConnectionSource::Params(config) => {
                assert!(matches!(
                    config.params.host.as_ref().unwrap().first(),
                    Some(ParamSource::Pattern { .. })
                ));
                assert!(matches!(
                    config.params.port.as_ref().unwrap().first(),
                    Some(ParamSource::Pattern { .. })
                ));
                assert!(matches!(
                    config.params.user.as_ref().unwrap().first(),
                    Some(ParamSource::Variable(_))
                ));
                assert!(matches!(
                    config.params.password.as_ref().unwrap().first(),
                    Some(ParamSource::Secret { .. })
                ));
            }
            other => panic!("expected Params, got {:?}", other),
        }
        assert_eq!(
            source,
            serde_json::from_str(&serde_json::to_string(&source).unwrap()).unwrap()
        );

        let multi_host: ConnectionSource = serde_json::from_str(
            r#"{
            "params": {
                "host": [
                    "WRITE_HOST",
                    { "env_var_name": "READ_SERVER", "value_pattern": "^([^:]+):" }
                ]
            }
        }"#,
        )
        .unwrap();
        match multi_host {
            ConnectionSource::Params(config) => {
                let hosts = config.params.host.as_ref().unwrap();
                assert_eq!(hosts.len(), 2);
                assert!(matches!(hosts.first(), Some(ParamSource::Variable(_))));
                assert!(matches!(hosts.get(1), Some(ParamSource::Pattern { .. })));
            }
            other => panic!("expected Params, got {:?}", other),
        }

        let multi_url: ConnectionSource =
            serde_json::from_str(r#"{ "url": ["DB_WRITE_URL", "DB_READ_URL"] }"#).unwrap();
        match multi_url {
            ConnectionSource::FlatUrl { url, .. } => assert_eq!(url.len(), 2),
            other => panic!("expected FlatUrl, got {:?}", other),
        }
    }

    fn base_with_ttl(ttl_secs: Option<u64>, ttl_mins: Option<u64>) -> DatabaseBranchBaseConfig {
        DatabaseBranchBaseConfig {
            id: None,
            name: None,
            ttl_secs,
            ttl_mins,
            creation_timeout_secs: 60,
            version: None,
            image: None,
            connection: ConnectionSource::FlatUrl {
                source_type: None,
                url: "DB_URL".to_owned().into(),
            },
        }
    }

    #[test]
    fn db_branch_resolved_ttl_prefers_minutes_when_only_minutes_set() {
        let base = base_with_ttl(None, Some(5));
        assert_eq!(base.resolved_ttl_secs(), 300);
    }

    #[test]
    fn db_branch_resolved_ttl_falls_back_to_default() {
        let base = base_with_ttl(None, None);
        assert_eq!(
            base.resolved_ttl_secs(),
            DatabaseBranchBaseConfig::DEFAULT_TTL_SECS
        );
    }

    #[test]
    fn db_branch_resolved_ttl_uses_seconds_when_set() {
        let base = base_with_ttl(Some(123), None);
        assert_eq!(base.resolved_ttl_secs(), 123);
    }

    #[test]
    fn db_branch_verify_rejects_both_ttl_fields() {
        let base = base_with_ttl(Some(120), Some(2));
        assert!(matches!(base.verify(), Err(ConfigError::Conflict(_))));
    }

    #[test]
    fn db_branch_verify_rejects_image_with_version() {
        let mut base = base_with_ttl(None, None);
        base.image = Some("registry.example.com/postgresql:15-partman".to_owned());
        base.verify().expect("image alone should verify");

        base.version = Some("15".to_owned());
        assert!(matches!(base.verify(), Err(ConfigError::Conflict(_))));
    }

    fn pg_branch_with_connection(connection: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Pg(Box::new(pg::PgBranchConfig {
            base: DatabaseBranchBaseConfig {
                id: None,
                name: None,
                ttl_secs: None,
                ttl_mins: None,
                creation_timeout_secs: 60,
                version: None,
                image: None,
                connection,
            },
            copy: Default::default(),
            connection_settings: Default::default(),
            iam_auth: None,
            migrations: None,
        }))
    }

    #[test]
    fn connection_env_keys_url_variant() {
        let branch = pg_branch_with_connection(ConnectionSource::Url {
            url: TargetEnvironmentVariableSource::Env {
                container: None,
                variable: "DB_URL".to_owned(),
                value: None,
            },
        });
        assert_eq!(branch.connection_env_keys(), vec!["DB_URL"]);
    }

    #[test]
    fn connection_env_keys_flat_url_variant() {
        let branch = pg_branch_with_connection(ConnectionSource::FlatUrl {
            source_type: Some(ConnectionSourceType::Env),
            url: vec!["WRITE_URL".to_owned(), "READ_URL".to_owned()].into(),
        });
        assert_eq!(branch.connection_env_keys(), vec!["WRITE_URL", "READ_URL"]);
    }

    #[test]
    fn connection_env_keys_params_variant_skips_secret_without_env_name() {
        let branch =
            pg_branch_with_connection(ConnectionSource::Params(Box::new(ConnectionParamsConfig {
                source_type: None,
                params: ConnectionParamsVars {
                    host: Some(ParamSource::Variable("DB_HOST".to_owned()).into()),
                    port: None,
                    user: Some(ParamSource::Variable("DB_USER".to_owned()).into()),
                    password: Some(
                        ParamSource::Secret {
                            name: "rds".to_owned(),
                            key: "password".to_owned(),
                            env_var_name: None,
                        }
                        .into(),
                    ),
                    database: Some(ParamSource::Variable("DB_NAME".to_owned()).into()),
                    extra: Default::default(),
                },
            })));
        assert_eq!(
            branch.connection_env_keys(),
            vec!["DB_HOST", "DB_USER", "DB_NAME"]
        );
    }
    #[test]
    fn connection_env_keys_redis() {
        let branch = DatabaseBranchConfig::Redis(Box::new(redis::RedisBranchConfig::Local(
            LocalRedisBranchConfig {
                id: None,
                connection: redis::RedisConnectionConfig {
                    url: Some(redis::RedisValueSource::Env(redis::RedisEnvSource {
                        source_type: redis::RedisEnvSourceType::Env,
                        variable: "REDIS_URL".to_owned(),
                        container: None,
                    })),
                    host: None,
                    port: None,
                    password: Some(redis::RedisValueSource::Direct("hunter2".to_owned())),
                    username: None,
                    database: None,
                    tls: None,
                },
                local: Default::default(),
            },
        )));
        assert_eq!(branch.connection_env_keys(), vec!["REDIS_URL"]);
    }
}
