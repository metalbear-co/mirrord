use std::{borrow::Cow, collections::BTreeMap, ops::Deref, path::PathBuf};

use fancy_regex::Regex;
use mirrord_analytics::{Analytics, CollectAnalytics};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize, ser::SerializeMap};
use strum::IntoEnumIterator;
use strum_macros::{EnumDiscriminants, EnumIter, IntoStaticStr};

use crate::config::{self, ConfigError};

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
pub mod cockroachdb;
pub mod dynamodb;
pub mod generic;
pub mod mariadb;
pub mod mongodb;
pub mod mssql;
pub mod mysql;
pub mod pg;
pub mod redis;
pub mod s3;
pub mod spanner;

pub use clickhouse::{
    ClickhouseBranchConfig, ClickhouseBranchCopyConfig, ClickhouseBranchTableCopyConfig,
};
pub use cockroachdb::{
    CockroachdbBranchConfig, CockroachdbBranchCopyConfig, CockroachdbBranchTableCopyConfig,
};
pub use dynamodb::{
    DynamodbBranchCollectionCopyConfig, DynamodbBranchConfig, DynamodbBranchCopyConfig,
};
pub use generic::{GenericBranchConfig, GenericCopyConfig, GenericReadinessConfig};
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
pub use s3::{S3BranchConfig, S3BranchCopyConfig, S3Provider};
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
        /// Resolved relative to the working directory. Mutually exclusive with `locations`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// Container image override for the migration runner.
        ///
        /// Required with `locations`, which point inside this image.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        /// Flyway locations inside `image` holding the migration files, for images with the
        /// SQL baked in (e.g. `filesystem:/flyway/sql`). Mutually exclusive with `path`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locations: Vec<String>,
    },
    /// Run a user-provided image as the migration job (e.g. an app image whose setup
    /// script runs the framework's migration command).
    Container {
        /// Full image reference for the migration container, including the tag.
        image: String,
        /// Entrypoint command override for the migration container.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        /// Entrypoint args override for the migration container.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        /// Extra environment variables for the migration container. Values can reference the
        /// injected `MIRRORD_DB_*` connection vars with Kubernetes `$(VAR)` expansion.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
}

impl SqlBranchMigrationsConfig {
    fn verify(&self, database: &DatabaseSourceConfig) -> Result<(), ConfigError> {
        if database.name.is_none() {
            const MESSAGE: &str = "`feature.db_branches[].migrations` requires `feature.db_branches[].name` to be set.";

            return Err(ConfigError::Conflict(MESSAGE.to_owned()));
        }

        let Self::Flyway {
            path,
            image,
            locations,
        } = self
        else {
            return Ok(());
        };

        match (path, locations.is_empty()) {
            (Some(_), false) => Err(ConfigError::Conflict(
                "`feature.db_branches[].migrations` accepts either `path` (local migration files) \
                 or `locations` (paths inside `image`), not both."
                    .to_owned(),
            )),
            (None, true) => Err(ConfigError::Conflict(
                "`feature.db_branches[].migrations` with `flavor: flyway` needs migration files: \
                 set `path` to a local directory, or `locations` to paths inside `image`."
                    .to_owned(),
            )),
            (None, false) if image.is_none() => Err(ConfigError::Conflict(
                "`feature.db_branches[].migrations.locations` points inside the migration image, \
                 so it requires `feature.db_branches[].migrations.image` to be set."
                    .to_owned(),
            )),
            _ => Ok(()),
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
    /// Counts branches matching a predicate. The building block for the usage
    /// analytics counters in [`CollectAnalytics`], so each new counter is one
    /// `count_branches` call instead of its own iteration method.
    fn count_branches(&self, matcher: impl Fn(&DatabaseBranchConfig) -> bool) -> usize {
        self.0.iter().filter(|db| matcher(db)).count()
    }

    /// Verifies invariants that span individual branch configs (e.g. `ttl_secs`/`ttl_mins`
    /// mutual exclusion).
    pub fn verify(&self, context: &mut config::ConfigContext) -> Result<(), ConfigError> {
        for branch in &self.0 {
            match branch {
                // Generic and Redis branches layer flavor rules on top of the shared ones -
                // and generic's image/version messages have to win over the shared ones - so
                // they verify themselves end to end.
                DatabaseBranchConfig::Generic(cfg) => cfg.verify(context)?,
                // MongoDB accepts only a subset of the shared `iam_auth` types, so it layers
                // that rule on top of the shared ones.
                DatabaseBranchConfig::Mongodb(cfg) => {
                    branch.verify_shared()?;

                    if matches!(cfg.iam_auth, Some(IamAuthConfig::GcpCloudSql { .. })) {
                        return Err(ConfigError::Conflict(
                            "`feature.db_branches[].iam_auth` with `type: gcp_cloud_sql` is not \
                             supported for MongoDB branches; only `aws_rds` (MONGODB-AWS) is."
                                .to_owned(),
                        ));
                    }
                }
                DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                    RedisBranchConfig::Local(_) => {}
                    RedisBranchConfig::Remote(remote) => remote.verify()?,
                },
                // S3 accepts only the `bucket` param, which the shared checks know nothing
                // about.
                DatabaseBranchConfig::S3(cfg) => cfg.verify()?,
                other => other.verify_shared()?,
            }
        }

        Ok(())
    }
}

/// Engine-agnostic seeding mode of a branch, used for usage analytics. Engines
/// without copy modes (generic, local Redis) have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchCopyMode {
    Empty,
    Schema,
    All,
}

impl DatabaseBranchConfig {
    /// The fields every branch shares, when the variant has them. Local Redis branches don't -
    /// mirrord runs them itself, so they never reach the operator and carry only an `id`.
    pub fn base(&self) -> Option<&BranchBaseConfig> {
        match self {
            DatabaseBranchConfig::Clickhouse(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Cockroachdb(cfg) => Some(&cfg.base),
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
            DatabaseBranchConfig::S3(cfg) => Some(&cfg.base),
            DatabaseBranchConfig::Spanner(cfg) => Some(&cfg.base),
        }
    }

    /// The image settings of this branch's pod, for the flavors the operator spawns in the
    /// cluster. [`None`] for flavors that have no pod of their own.
    pub fn pod(&self) -> Option<&BranchPodConfig> {
        match self {
            DatabaseBranchConfig::Clickhouse(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Cockroachdb(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Dynamodb(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Generic(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Mariadb(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Mongodb(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Mssql(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Mysql(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Pg(cfg) => Some(&cfg.pod),
            DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                RedisBranchConfig::Local(_) => None,
                RedisBranchConfig::Remote(remote) => Some(&remote.pod),
            },
            // An S3 branch is a bucket in the provider's cloud, not a server mirrord runs.
            DatabaseBranchConfig::S3(_) => None,
            DatabaseBranchConfig::Spanner(cfg) => Some(&cfg.pod),
        }
    }

    /// The source database this branch is made from, for the connection-based flavors.
    /// [`None`] for flavors that locate their source some other way.
    pub fn database(&self) -> Option<&DatabaseSourceConfig> {
        match self {
            DatabaseBranchConfig::Clickhouse(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Cockroachdb(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Dynamodb(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Generic(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Mariadb(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Mongodb(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Mssql(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Mysql(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Pg(cfg) => Some(&cfg.database),
            DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                RedisBranchConfig::Local(_) => None,
                RedisBranchConfig::Remote(remote) => Some(&remote.database),
            },
            // An S3 branch has no server hosting many databases; its source is a single
            // bucket, located by the `bucket` param of `S3BranchConfig::source`.
            DatabaseBranchConfig::S3(_) => None,
            DatabaseBranchConfig::Spanner(cfg) => Some(&cfg.database),
        }
    }

    /// [`Self::database`], for the operator client rewriting literal connection values into
    /// Secret references before the config leaves the machine.
    pub fn database_mut(&mut self) -> Option<&mut DatabaseSourceConfig> {
        match self {
            DatabaseBranchConfig::Clickhouse(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Cockroachdb(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Dynamodb(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Generic(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Mariadb(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Mongodb(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Mssql(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Mysql(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Pg(cfg) => Some(&mut cfg.database),
            DatabaseBranchConfig::Redis(cfg) => match &mut **cfg {
                RedisBranchConfig::Local(_) => None,
                RedisBranchConfig::Remote(remote) => Some(&mut remote.database),
            },
            DatabaseBranchConfig::S3(_) => None,
            DatabaseBranchConfig::Spanner(cfg) => Some(&mut cfg.database),
        }
    }

    /// The schema migrations to run once the branch is up, for the SQL flavors that support
    /// them.
    pub fn migrations(&self) -> Option<&SqlBranchMigrationsConfig> {
        match self {
            DatabaseBranchConfig::Cockroachdb(cfg) => cfg.migrations.as_ref(),
            DatabaseBranchConfig::Mariadb(cfg) => cfg.migrations.as_ref(),
            DatabaseBranchConfig::Mssql(cfg) => cfg.migrations.as_ref(),
            DatabaseBranchConfig::Mysql(cfg) => cfg.migrations.as_ref(),
            DatabaseBranchConfig::Pg(cfg) => cfg.migrations.as_ref(),
            DatabaseBranchConfig::Clickhouse(_)
            | DatabaseBranchConfig::Dynamodb(_)
            | DatabaseBranchConfig::Generic(_)
            | DatabaseBranchConfig::Mongodb(_)
            | DatabaseBranchConfig::Redis(_)
            | DatabaseBranchConfig::S3(_)
            | DatabaseBranchConfig::Spanner(_) => None,
        }
    }

    /// Verifies the field groups this branch shares with the others. Flavors with rules of
    /// their own call this from their own `verify` instead.
    fn verify_shared(&self) -> Result<(), ConfigError> {
        if let Some(base) = self.base() {
            base.verify()?;
        }

        if let Some(pod) = self.pod() {
            pod.verify()?;
        }

        if let Some((migrations, database)) = self.migrations().zip(self.database()) {
            migrations.verify(database)?;
        }

        Ok(())
    }

    /// The engine-agnostic copy mode of this branch, or [`None`] for engines that
    /// have no copy modes (generic, local Redis).
    pub fn copy_mode(&self) -> Option<BranchCopyMode> {
        let mode = match self {
            DatabaseBranchConfig::Clickhouse(cfg) => match cfg.copy {
                ClickhouseBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                ClickhouseBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                ClickhouseBranchCopyConfig::All => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Cockroachdb(cfg) => match cfg.copy {
                CockroachdbBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                CockroachdbBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                CockroachdbBranchCopyConfig::All => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Dynamodb(cfg) => match cfg.copy {
                DynamodbBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                DynamodbBranchCopyConfig::All { .. } => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Generic(_) => return None,
            DatabaseBranchConfig::Mariadb(cfg) => match cfg.copy {
                MariadbBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                MariadbBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                MariadbBranchCopyConfig::All { .. } => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Mongodb(cfg) => match cfg.copy {
                MongodbBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                MongodbBranchCopyConfig::All { .. } => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Mssql(cfg) => match cfg.copy {
                MssqlBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                MssqlBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                MssqlBranchCopyConfig::All => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Mysql(cfg) => match cfg.copy {
                MysqlBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                MysqlBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                MysqlBranchCopyConfig::All { .. } => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Pg(cfg) => match cfg.copy {
                PgBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                PgBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                PgBranchCopyConfig::All { .. } => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                RedisBranchConfig::Local(_) => return None,
                RedisBranchConfig::Remote(remote) => match remote.copy {
                    RedisBranchCopyConfig::Empty => BranchCopyMode::Empty,
                    RedisBranchCopyConfig::All { .. } => BranchCopyMode::All,
                },
            },
            DatabaseBranchConfig::S3(cfg) => match cfg.copy {
                S3BranchCopyConfig::Empty => BranchCopyMode::Empty,
                S3BranchCopyConfig::All { .. } => BranchCopyMode::All,
            },
            DatabaseBranchConfig::Spanner(cfg) => match cfg.copy {
                SpannerBranchCopyConfig::Empty { .. } => BranchCopyMode::Empty,
                SpannerBranchCopyConfig::Schema { .. } => BranchCopyMode::Schema,
                SpannerBranchCopyConfig::All => BranchCopyMode::All,
            },
        };

        Some(mode)
    }

    /// The individual connection params of this branch, when its source is
    /// declared as params rather than a URL.
    fn connection_params(&self) -> Option<&ConnectionParamsVars> {
        match self {
            // An S3 branch is always params-shaped; a bucket has no connection URL.
            DatabaseBranchConfig::S3(cfg) => Some(&cfg.source.params),
            other => match &other.database()?.connection {
                ConnectionSource::Params(config) => Some(&config.params),
                ConnectionSource::Url { .. } | ConnectionSource::FlatUrl { .. } => None,
            },
        }
    }

    /// True when any of this branch's source values is read from a Kubernetes Secret or from
    /// Google Secret Manager rather than from the target pod's environment.
    fn uses_secret(&self) -> bool {
        match self {
            DatabaseBranchConfig::S3(cfg) => cfg.source.params.uses_secret(),
            other => other
                .database()
                .is_some_and(|database| database.connection.uses_secret()),
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
            // The operator redirects only the host/port vars of a generic branch; the app's
            // other vars (user/password/database/extras) are deliberately left untouched.
            DatabaseBranchConfig::Generic(cfg) => cfg.collect_redirected_env_keys(&mut keys),
            // Spanner leaves the app's project/instance/database vars untouched; the operator
            // only injects the emulator host var, so that is the sole redirected key.
            DatabaseBranchConfig::Spanner(cfg) => keys.push(cfg.emulator_host.as_str()),
            // The bucket var is the one key an S3 branch has, and the operator repoints it at
            // the branch bucket. It lives in `extra`, which the shared collector skips.
            DatabaseBranchConfig::S3(cfg) => {
                for source in cfg.source.params.extra.values().flatten() {
                    source.collect_env_keys(&mut keys);
                }
            }
            // A local Redis branch is redirected by the CLI rather than the operator, from
            // its own connection block.
            DatabaseBranchConfig::Redis(cfg) => match &**cfg {
                RedisBranchConfig::Local(local) => local.connection.collect_env_keys(&mut keys),
                RedisBranchConfig::Remote(remote) => {
                    remote.database.connection.collect_env_keys(&mut keys)
                }
            },
            other => {
                if let Some(database) = other.database() {
                    database.connection.collect_env_keys(&mut keys);
                }
            }
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

    fn is_url(&self) -> bool {
        matches!(self, Self::Url { .. } | Self::FlatUrl { .. })
    }

    /// True when any connection value is read from a Kubernetes Secret or an
    /// external secret manager (GCP/AWS) rather than the target pod's environment.
    fn uses_secret(&self) -> bool {
        match self {
            Self::Url { url } => matches!(
                url,
                TargetEnvironmentVariableSource::Secret { .. }
                    | TargetEnvironmentVariableSource::GcpSecretManager { .. }
                    | TargetEnvironmentVariableSource::AwsSecretsManager { .. }
            ),
            Self::FlatUrl { .. } => false,
            Self::Params(config) => config.params.uses_secret(),
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
            }
            | Self::AwsSecretsManager {
                env_var_name: Some(name),
                ..
            } => out.push(name),
            Self::Secret {
                env_var_name: None, ..
            }
            | Self::GcpSecretManager {
                env_var_name: None, ..
            }
            | Self::AwsSecretsManager {
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

    /// Every declared param source: the fixed slots plus the engine-specific extras.
    /// Unlike [`Self::collect_env_keys`], extras are included - they matter for
    /// analytics even though most of them are not redirected locally.
    fn all_sources(&self) -> impl Iterator<Item = &ParamSource> {
        [
            &self.host,
            &self.port,
            &self.user,
            &self.password,
            &self.database,
        ]
        .into_iter()
        .filter_map(Option::as_ref)
        .chain(self.extra.values())
        .flat_map(|sources| sources.iter())
    }

    /// True when any param is read from a Kubernetes Secret or an external secret manager
    /// (GCP/AWS) rather than from the target pod's environment.
    fn uses_secret(&self) -> bool {
        self.all_sources().any(ParamSource::is_secret)
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
/// The fields below are shared by every engine. Not every engine has every one of them: an
/// engine that mirrord does not spawn as a pod in the cluster takes no `image`/`version`, and
/// one that is not reached over a connection to a server hosting many databases takes no
/// `name` and locates its source its own way. Engine-specific fields (copy modes, `iam_auth`,
/// `connection_settings`, `emulator_host`) are documented under each `type`.
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
/// #### feature.db_branches[].profile (type: clickhouse, cockroachdb, dynamodb, generic, mariadb, mongodb, mssql, mysql, pg, redis, s3, spanner) {#feature-db_branches-sql-profile}
///
/// Name of an operator branch-config profile to use for this branch. Cluster admins can define
/// named profiles under the per-database `profiles` map in the operator's Helm values
/// (e.g. `redisBranchConfig.profiles`), each carrying its own pod settings such as TLS mode,
/// server arguments, pull secrets, and allowed images. When `profile` is not set, the
/// operator's default branch config applies. Referencing a profile the operator does not
/// define fails the branch with an error listing the available profiles.
///
/// ```json
/// { "type": "redis", "profile": "telapp", "connection": { "url": "REDIS_URL" } }
/// ```
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
/// Schema migrations to run on the branch after it is created. The `flavor` field selects how
/// they run.
///
/// [Flyway](https://documentation.red-gate.com/flyway) with a local migrations directory:
///
/// ```json
/// { "migrations": { "flavor": "flyway", "path": "./migrations" } }
/// ```
///
/// - `path`: local directory holding the migration files, resolved relative to the working
///   directory.
/// - `image`: optional container image override for the migration runner.
///
/// Flyway with the SQL baked into the job image, running against in-image paths:
///
/// ```json
/// {
///   "migrations": {
///     "flavor": "flyway",
///     "image": "registry.example.com/my-migrations:latest",
///     "locations": ["filesystem:/flyway/sql"]
///   }
/// }
/// ```
///
/// - `locations`: Flyway locations inside `image` holding the migration files. Mutually exclusive
///   with `path`, and requires `image`.
///
/// A user-provided image and command, for apps that ship migrations in their own image
/// (e.g. a setup script that runs the framework's migration command):
///
/// ```json
/// {
///   "migrations": {
///     "flavor": "container",
///     "image": "registry.example.com/my-app:latest",
///     "command": ["./db_setup.sh"],
///     "env": {
///       "DATABASE_URL": "mysql://$(MIRRORD_DB_USER):$(MIRRORD_DB_PASSWORD)@$(MIRRORD_DB_HOST):$(MIRRORD_DB_PORT)/$(MIRRORD_DB_NAME)"
///     }
///   }
/// }
/// ```
///
/// - `image`: full image reference for the migration container, including the tag.
/// - `command`/`args`: optional entrypoint overrides; when unset, the image's own entrypoint runs.
/// - `env`: extra environment variables. The operator injects the branch connection as
///   `MIRRORD_DB_HOST`, `MIRRORD_DB_PORT`, `MIRRORD_DB_USER`, `MIRRORD_DB_PASSWORD`, and
///   `MIRRORD_DB_NAME`; `env` values (and `command`/`args`) can reference them with Kubernetes
///   `$(VAR)` expansion.
///
/// Requires [`name`](#feature-db_branches-sql-name) to be set.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(
    name(DatabaseBranchEngine),
    derive(EnumIter, IntoStaticStr),
    strum(serialize_all = "lowercase")
)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum DatabaseBranchConfig {
    Clickhouse(Box<ClickhouseBranchConfig>),
    Cockroachdb(Box<CockroachdbBranchConfig>),
    Dynamodb(Box<DynamodbBranchConfig>),
    Generic(Box<GenericBranchConfig>),
    Mariadb(Box<MariadbBranchConfig>),
    Mongodb(Box<MongodbBranchConfig>),
    Mssql(Box<MssqlBranchConfig>),
    Mysql(Box<MysqlBranchConfig>),
    Pg(Box<PgBranchConfig>),
    Redis(Box<RedisBranchConfig>),
    S3(Box<S3BranchConfig>),
    Spanner(Box<SpannerBranchConfig>),
}

/// <!--${internal}-->
/// The fields every branch has, whatever it branches.
///
/// This is the one group a flavor always carries.
///
/// The fields are documented once on [`DatabaseBranchConfig`] so the generated config docs do
/// not repeat them for each engine; keep only short schema descriptions here.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct BranchBaseConfig {
    /// Optional stable id for reusing or sharing a branch across users.
    pub id: Option<String>,

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

    /// Name of an admin-defined operator branch-config profile to use for this branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl Default for BranchBaseConfig {
    fn default() -> Self {
        Self {
            id: None,
            ttl_secs: None,
            ttl_mins: None,
            creation_timeout_secs: default_creation_timeout_secs(),
            profile: None,
        }
    }
}

impl BranchBaseConfig {
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

        Ok(())
    }
}

/// <!--${internal}-->
/// Picks the image for branches the operator spawns as pods in the cluster.
///
/// Flavors whose branch only ever exists in the provider's cloud have no pod to configure and
/// leave this group out. Documented on [`DatabaseBranchConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct BranchPodConfig {
    /// Source database image version. Defaults to the operator's built-in version.
    pub version: Option<String>,

    /// Full image reference for the branch container, including the tag. Overrides the
    /// operator-configured registry entirely. Mutually exclusive with `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl BranchPodConfig {
    pub fn verify(&self) -> Result<(), ConfigError> {
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

/// <!--${internal}-->
/// Locates the source database of a branch, for engines that are reached over a connection
/// and serve more than one database.
///
/// Documented on [`DatabaseBranchConfig`].
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct DatabaseSourceConfig {
    /// Source database name, used when the operator cannot read it from the connection.
    pub name: Option<String>,

    /// How to source the connection info for the source database. The operator swaps it for
    /// the branch's connection once the branch is ready.
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
/// The `variable` names the key in the credential Secret that the CLI creates, and is
/// required - a value-only object does not deserialize.
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
    /// Value fetched from AWS Secrets Manager at branch data-copy time by the
    /// init container, using the target pod's service account (IRSA / EKS Pod
    /// Identity). `aws_secrets_manager` is a secret name or full ARN, passed
    /// verbatim to `GetSecretValue`. mirrord does not read the value; only the
    /// branch init container does, so the operator needs no access to the secret.
    ///
    /// Setup: the branch pod inherits the target pod's service account, so that
    /// account's IAM role must allow `secretsmanager:GetSecretValue` on the
    /// secret. No operator-level permissions are required.
    ///
    /// Add `env_var_name` to also point the local app at the branch DB under that
    /// name (same semantics as `Secret`). Without it the value is only used to
    /// provision the branch and the local app keeps reading its own source.
    AwsSecretsManager {
        #[serde(rename = "aws_secrets_manager")]
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
            Self::Secret { .. }
            | Self::GcpSecretManager { .. }
            | Self::AwsSecretsManager { .. } => None,
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
            }
            | ParamSource::AwsSecretsManager {
                env_var_name: Some(name),
                ..
            } => out.push(name),
            ParamSource::Secret {
                env_var_name: None, ..
            }
            | ParamSource::GcpSecretManager {
                env_var_name: None, ..
            }
            | ParamSource::AwsSecretsManager {
                env_var_name: None, ..
            } => {}
        }
    }

    pub fn is_secret(&self) -> bool {
        match self {
            Self::Variable(_) | Self::Pattern { .. } | Self::Env { .. } => false,
            Self::Secret { .. }
            | Self::GcpSecretManager { .. }
            | Self::AwsSecretsManager { .. } => true,
        }
    }

    /// The regex a `value_pattern` source extracts its param with; `None` when the whole
    /// env var value is the param.
    pub fn value_pattern(&self) -> Option<&str> {
        match self {
            Self::Pattern { value_pattern, .. } => Some(value_pattern),
            _ => None,
        }
    }
}

/// The span of `value` that a `value_pattern` regex designates for the given param.
///
/// The operator rewrites exactly this span when it points the env var at a branch, so
/// reading the param back out of a rewritten value must pick the same capture: the group
/// named after the param (e.g. `(?P<host>...)` for `host`), then `(?P<value>...)`, then the
/// first unnamed group. Returns `None` when the pattern does not compile or does not match -
/// the operator validates patterns at branch creation, so either means the value at hand is
/// not the one the pattern was written for.
pub fn extract_pattern_param<'v>(
    value: &'v str,
    pattern: &str,
    param_name: &str,
) -> Option<&'v str> {
    let regex = Regex::new(pattern).ok()?;
    let captures = regex.captures(value).ok().flatten()?;
    captures
        .name(param_name)
        .or_else(|| captures.name("value"))
        .or_else(|| captures.get(1))
        .map(|capture| capture.as_str())
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
    /// nesting. The operator resolves each from the target pod and hands it to the branch init
    /// sidecar. A param with a branch-side equivalent (PostgreSQL's and CockroachDB's `sslmode`,
    /// an S3 branch's `bucket`) also gets its env var rewritten on the local app to the branch's
    /// own value; the rest are read-only source locators the local app keeps untouched.
    ///
    /// PostgreSQL and CockroachDB accept `sslmode`: the TLS mode of the source connection,
    /// which params mode has no URL to carry.
    ///
    /// An S3 branch accepts `bucket`: the name of the source bucket, which the operator repoints
    /// at the branch bucket once that exists.
    ///
    /// Google Cloud Spanner keys name the env vars on the target pod that hold its three
    /// separate source identifiers:
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
/// - `aws_secrets_manager` fetched from AWS Secrets Manager by the init container.
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
    /// Fetched from AWS Secrets Manager by the branch init container using the
    /// target pod's service account. Same semantics as `ParamSource::AwsSecretsManager`.
    AwsSecretsManager {
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

/// Usage analytics for the db branching feature.
///
/// Every value is a branch count, because [`mirrord_analytics::AnalyticValue`]
/// deliberately carries no strings: each config trait dashboards care about becomes
/// its own counter instead of a labeled field. Keys are wire-stable once shipped -
/// append new counters, do not rename or repurpose existing ones.
///
/// Whether an admin-configured default image or registry applied to a branch is only
/// known operator-side; the closest client-side signal is `profile_count` (branches
/// referencing an admin-defined profile).
impl CollectAnalytics for &DatabaseBranchesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        // Per-engine counters come from the [`DatabaseBranchEngine`] discriminants, so
        // a newly added engine gets its counter without anyone remembering to add it.
        for engine in DatabaseBranchEngine::iter() {
            analytics.add(
                format!("{}_branch_count", <&'static str>::from(engine)),
                self.count_branches(|db| DatabaseBranchEngine::from(db) == engine),
            );
        }

        analytics.add(
            "copy_empty_count",
            self.count_branches(|db| db.copy_mode() == Some(BranchCopyMode::Empty)),
        );
        analytics.add(
            "copy_schema_count",
            self.count_branches(|db| db.copy_mode() == Some(BranchCopyMode::Schema)),
        );
        analytics.add(
            "copy_all_count",
            self.count_branches(|db| db.copy_mode() == Some(BranchCopyMode::All)),
        );

        analytics.add(
            "connection_url_count",
            self.count_branches(|db| db.database().is_some_and(|db| db.connection.is_url())),
        );
        analytics.add(
            "connection_params_count",
            self.count_branches(|db| db.connection_params().is_some()),
        );
        analytics.add(
            "connection_secret_count",
            self.count_branches(DatabaseBranchConfig::uses_secret),
        );

        analytics.add(
            "params_host_count",
            self.count_branches(|db| {
                db.connection_params()
                    .is_some_and(|params| params.host.is_some())
            }),
        );
        analytics.add(
            "params_port_count",
            self.count_branches(|db| {
                db.connection_params()
                    .is_some_and(|params| params.port.is_some())
            }),
        );
        analytics.add(
            "params_user_count",
            self.count_branches(|db| {
                db.connection_params()
                    .is_some_and(|params| params.user.is_some())
            }),
        );
        analytics.add(
            "params_password_count",
            self.count_branches(|db| {
                db.connection_params()
                    .is_some_and(|params| params.password.is_some())
            }),
        );
        analytics.add(
            "params_database_count",
            self.count_branches(|db| {
                db.connection_params()
                    .is_some_and(|params| params.database.is_some())
            }),
        );
        // Engine-specific params outside the fixed slots (e.g. Spanner's locators).
        analytics.add(
            "params_extra_count",
            self.count_branches(|db| {
                db.connection_params()
                    .is_some_and(|params| !params.extra.is_empty())
            }),
        );

        // Generic branches are excluded: `image` is required there, so counting them
        // would inflate a counter meant to measure opting into an image override.
        analytics.add(
            "user_image_count",
            self.count_branches(|db| {
                !matches!(db, DatabaseBranchConfig::Generic(_))
                    && db.pod().is_some_and(|pod| pod.image.is_some())
            }),
        );
        analytics.add(
            "profile_count",
            self.count_branches(|db| db.base().is_some_and(|base| base.profile.is_some())),
        );
    }
}

pub fn default_creation_timeout_secs() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rstest::rstest;
    use serde_json::{Value, json};

    use super::*;

    /// The capture priority must match the operator's substitution helper exactly - the
    /// operator rewrites the span this function reads back, so a priority mismatch would
    /// extract a span the operator never touched.
    #[test]
    fn pattern_param_capture_priority() {
        let url = "postgresql://root@10.0.0.5:26257/appdb";

        // Param-named group wins.
        assert_eq!(
            extract_pattern_param(url, "@(?P<host>[^:/]+)", "host"),
            Some("10.0.0.5")
        );
        // `value` group when no param-named group exists.
        assert_eq!(
            extract_pattern_param(url, ":(?P<value>[0-9]+)/", "port"),
            Some("26257")
        );
        // First unnamed group as the fallback.
        assert_eq!(
            extract_pattern_param(url, ":([0-9]+)/", "port"),
            Some("26257")
        );
        // Param-named group beats an earlier unnamed one.
        assert_eq!(
            extract_pattern_param(url, "(postgresql)://root@(?P<host>[^:/]+)", "host"),
            Some("10.0.0.5")
        );
        // No match and invalid pattern both come back empty instead of erroring.
        assert_eq!(
            extract_pattern_param("no url here", ":([0-9]+)/", "port"),
            None
        );
        assert_eq!(extract_pattern_param(url, "(unclosed", "port"), None);
    }

    /// Verifies that database configs properly deserialize.
    ///
    /// Tests all flavors except [`DatabaseBranchEngine::Redis`] and
    /// [`DatabaseBranchEngine::S3`], which are verified in [`redis_deserialize_compat`] and
    /// [`s3_deserialize_compat`].
    #[rstest]
    fn deserialize_compat(
        #[values(
            DatabaseBranchEngine::Clickhouse,
            DatabaseBranchEngine::Cockroachdb,
            DatabaseBranchEngine::Dynamodb,
            DatabaseBranchEngine::Generic,
            DatabaseBranchEngine::Mariadb,
            DatabaseBranchEngine::Mongodb,
            DatabaseBranchEngine::Mssql,
            DatabaseBranchEngine::Mysql,
            DatabaseBranchEngine::Pg,
            DatabaseBranchEngine::Redis,
            DatabaseBranchEngine::Spanner
        )]
        engine: DatabaseBranchEngine,
    ) {
        // Exhaustive on purpose: a new flavor cannot be added without saying what one of its
        // configs looks like, which is the prompt to give it a `#[values]` case above too.
        let (name, flavor_fields) = match engine {
            // A Redis branch's `name` picks a numbered database on the branch server, so it
            // has to parse as a number.
            DatabaseBranchEngine::Redis => ("3", json!({})),
            // Generic branches have no default image, take the listening port explicitly, and
            // accept only a params-mode connection.
            DatabaseBranchEngine::Generic => (
                "my-database",
                json!({
                    "port": 8086,
                    "connection": { "params": { "host": "DB_HOST", "port": "DB_PORT" } },
                }),
            ),
            DatabaseBranchEngine::Clickhouse
            | DatabaseBranchEngine::Cockroachdb
            | DatabaseBranchEngine::Dynamodb
            | DatabaseBranchEngine::Mariadb
            | DatabaseBranchEngine::Mongodb
            | DatabaseBranchEngine::Mssql
            | DatabaseBranchEngine::Mysql
            | DatabaseBranchEngine::Pg
            | DatabaseBranchEngine::Spanner => ("my-database", json!({})),
            // An S3 branch has neither a pod nor a source database, and names its source
            // `source` rather than `connection`, so none of the shared assertions below fit.
            DatabaseBranchEngine::S3 => unreachable!("checked in `s3_deserialize_compat`"),
        };

        let (Value::Object(mut fields), Value::Object(flavor_fields)) = (
            json!({
                "type": <&'static str>::from(engine),
                "id": "my-branch",
                "name": name,
                "ttl_mins": 5,
                "creation_timeout_secs": 90,
                "image": "registry.example.com/db:1",
                "profile": "telapp",
                "connection": { "url": { "type": "env", "variable": "DB_URL" } },
            }),
            flavor_fields,
        ) else {
            unreachable!("both are `json!` object literals")
        };
        fields.extend(flavor_fields);

        let expected_connection = serde_json::from_value::<ConnectionSource>(
            fields.get("connection").expect("set above").clone(),
        )
        .expect("the connection is a valid source on its own");

        let config = Value::Object(fields);
        let branch = serde_json::from_value::<DatabaseBranchConfig>(config.clone())
            .unwrap_or_else(|error| panic!("`{config}` should parse: {error}"));
        assert_eq!(DatabaseBranchEngine::from(&branch), engine);

        let base = branch.base().expect("every flavor here has the base group");
        assert_eq!(base.id.as_deref(), Some("my-branch"));
        assert_eq!(base.ttl_mins, Some(5));
        assert_eq!(base.resolved_ttl_secs(), 300);
        assert_eq!(base.creation_timeout_secs, 90);
        assert_eq!(base.profile.as_deref(), Some("telapp"));

        let pod = branch.pod().expect("every flavor here runs as a pod");
        assert_eq!(pod.image.as_deref(), Some("registry.example.com/db:1"));
        assert_eq!(pod.version, None);

        let database = branch
            .database()
            .expect("every flavor here branches a source database");
        assert_eq!(database.name.as_deref(), Some(name));
        assert_eq!(database.connection, expected_connection);

        DatabaseBranchesConfig(vec![branch.clone()])
            .verify(&mut config::ConfigContext::default())
            .expect("config should verify");

        let reparsed =
            serde_json::from_value::<DatabaseBranchConfig>(serde_json::to_value(&branch).unwrap())
                .expect("a serialized branch should parse back");
        assert_eq!(reparsed, branch);
    }

    /// Checks that [`RedisBranchConfig`] properly deserializes.
    ///
    /// A local Redis branch is the one config with no shared groups at all,
    /// so it is checked here, separately from [`deserialize_compat`] above.
    #[test]
    fn redis_deserialize_compat() {
        let config = json!({
            "type": "redis",
            "location": "local",
            "id": "my-branch",
            "connection": { "url": { "type": "env", "variable": "REDIS_URL" } },
            "local": { "port": 6380 },
        });

        let branch = serde_json::from_value::<DatabaseBranchConfig>(config).unwrap();
        assert_eq!(branch.base(), None);
        assert_eq!(branch.pod(), None);
        assert_eq!(branch.database(), None);
        assert_eq!(branch.connection_env_keys(), vec!["REDIS_URL"]);

        let DatabaseBranchConfig::Redis(redis) = &branch else {
            panic!("expected a Redis branch");
        };
        let RedisBranchConfig::Local(local) = &**redis else {
            panic!("expected a local Redis branch");
        };
        assert_eq!(local.id.as_deref(), Some("my-branch"));
        assert_eq!(local.local.port, 6380);
    }

    /// Checks that [`S3BranchConfig`] properly deserializes.
    ///
    /// S3 is the one flavor with no pod and no source database, so it is checked here rather
    /// than in [`deserialize_compat`].
    #[test]
    fn s3_deserialize_compat() {
        let config = json!({
            "type": "s3",
            "provider": "AWS",
            "id": "my-branch",
            "ttl_mins": 5,
            "creation_timeout_secs": 90,
            "profile": "telapp",
            "source": { "params": { "bucket": "MY_BUCKET_ENV_VAR" } },
            "copy": { "mode": "all", "objects": ["^fixtures/.*"] },
        });

        let branch = serde_json::from_value::<DatabaseBranchConfig>(config).unwrap();
        assert_eq!(
            DatabaseBranchEngine::from(&branch),
            DatabaseBranchEngine::S3
        );
        assert_eq!(branch.pod(), None);
        assert_eq!(branch.database(), None);
        assert_eq!(branch.copy_mode(), Some(BranchCopyMode::All));
        assert_eq!(branch.connection_env_keys(), vec!["MY_BUCKET_ENV_VAR"]);

        let base = branch.base().expect("S3 branches carry the base group");
        assert_eq!(base.id.as_deref(), Some("my-branch"));
        assert_eq!(base.resolved_ttl_secs(), 300);
        assert_eq!(base.creation_timeout_secs, 90);
        assert_eq!(base.profile.as_deref(), Some("telapp"));

        let DatabaseBranchConfig::S3(s3) = &branch else {
            panic!("expected an S3 branch");
        };
        assert_eq!(s3.provider, S3Provider::Aws);
        assert_eq!(
            s3.source.params.extra.get("bucket").and_then(|b| b.first()),
            Some(&ParamSource::Variable("MY_BUCKET_ENV_VAR".to_owned()))
        );
        assert_eq!(
            s3.copy,
            S3BranchCopyConfig::All {
                objects: vec!["^fixtures/.*".to_owned()],
            }
        );

        DatabaseBranchesConfig(vec![branch.clone()])
            .verify(&mut config::ConfigContext::default())
            .expect("config should verify");

        let reparsed =
            serde_json::from_value::<DatabaseBranchConfig>(serde_json::to_value(&branch).unwrap())
                .expect("a serialized branch should parse back");
        assert_eq!(reparsed, branch);
    }

    /// The minimal S3 config: no provider (AWS is the default), no copy mode (empty is), and
    /// the source under either of its two accepted names.
    #[rstest]
    #[case::source("source")]
    #[case::connection("connection")]
    fn s3_minimal_config(#[case] source_field: &str) {
        let config = json!({
            "type": "s3",
            source_field: { "params": { "bucket": "MY_BUCKET_ENV_VAR" } },
        });

        let branch = serde_json::from_value::<DatabaseBranchConfig>(config).unwrap();
        let DatabaseBranchConfig::S3(s3) = &branch else {
            panic!("expected an S3 branch");
        };
        assert_eq!(s3.provider, S3Provider::Aws);
        assert_eq!(s3.copy, S3BranchCopyConfig::Empty);
        assert_eq!(
            s3.base.resolved_ttl_secs(),
            BranchBaseConfig::DEFAULT_TTL_SECS
        );
        assert_eq!(branch.copy_mode(), Some(BranchCopyMode::Empty));

        DatabaseBranchesConfig(vec![branch])
            .verify(&mut config::ConfigContext::default())
            .expect("config should verify");
    }

    /// An S3 branch takes exactly one param, so anything else is a config error rather than a
    /// branch the operator would reject later.
    #[rstest]
    #[case::no_bucket(json!({}))]
    #[case::unknown_param(json!({ "bucket": "BUCKET", "table": "TABLE" }))]
    #[case::fixed_slot(json!({ "bucket": "BUCKET", "host": "HOST" }))]
    fn s3_verify_rejects_params_other_than_bucket(#[case] params: Value) {
        let branch = serde_json::from_value::<DatabaseBranchConfig>(json!({
            "type": "s3",
            "source": { "params": params },
        }))
        .expect("params are only checked by `verify`");

        DatabaseBranchesConfig(vec![branch])
            .verify(&mut config::ConfigContext::default())
            .expect_err("only `bucket` is a valid S3 param");
    }

    /// The bucket param is as flexible as any other engine's: `env_from` resolution and a
    /// Secret-backed value both parse, and the Secret is picked up by the usage analytics.
    #[test]
    fn s3_source_accepts_env_from_and_secrets() {
        let branch = serde_json::from_value::<DatabaseBranchConfig>(json!({
            "type": "s3",
            "source": {
                "type": "env_from",
                "params": {
                    "bucket": { "secret": "my-secret", "key": "bucket", "env_var_name": "MY_BUCKET_ENV_VAR" },
                },
            },
        }))
        .unwrap();

        let DatabaseBranchConfig::S3(s3) = &branch else {
            panic!("expected an S3 branch");
        };
        assert_eq!(s3.source.source_type, Some(ConnectionSourceType::EnvFrom));
        assert!(s3.source.params.uses_secret());
        assert_eq!(branch.connection_env_keys(), vec!["MY_BUCKET_ENV_VAR"]);

        DatabaseBranchesConfig(vec![branch])
            .verify(&mut config::ConfigContext::default())
            .expect("config should verify");
    }

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
        let ConnectionSource::Params(config) = &spanner.database.connection else {
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
    fn deserialize_pg_query_params_roundtrip() {
        let json = serde_json::json!({
            "type": "pg",
            "connection": { "url": "DB_URL" },
            "query_params": { "sslmode": "disable" }
        });
        let branch: DatabaseBranchConfig = serde_json::from_value(json).unwrap();
        let DatabaseBranchConfig::Pg(pg) = &branch else {
            panic!("expected a pg branch, got {branch:?}");
        };
        assert_eq!(
            pg.query_params,
            BTreeMap::from([("sslmode".to_owned(), "disable".to_owned())])
        );

        let serialized = serde_json::to_value(&branch).unwrap();
        let roundtripped: DatabaseBranchConfig = serde_json::from_value(serialized).unwrap();
        assert_eq!(branch, roundtripped);
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
    fn serialize_roundtrip_url_aws_secrets_manager() {
        let source = ConnectionSource::Url {
            url: TargetEnvironmentVariableSource::AwsSecretsManager {
                secret_ref: "arn:aws:secretsmanager:us-east-1:123456789012:secret:db-url"
                    .to_owned(),
                env_var_name: Some("DATABASE_URL".to_owned()),
            },
        };
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized, "json was: {json}");
    }

    #[test]
    fn serialize_roundtrip_params_aws_secrets_manager() {
        let source = ConnectionSource::Params(Box::new(ConnectionParamsConfig {
            source_type: None,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("DB_HOST".to_owned()).into()),
                port: None,
                user: None,
                password: Some(
                    ParamSource::AwsSecretsManager {
                        secret_ref: "my-db-password".to_owned(),
                        env_var_name: Some("DB_PASSWORD".to_owned()),
                    }
                    .into(),
                ),
                database: None,
                extra: Default::default(),
            },
        }));
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: ConnectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, deserialized, "json was: {json}");
    }

    #[test]
    fn mongodb_iam_auth_parses_and_gcp_is_rejected() {
        let branch: DatabaseBranchConfig = serde_json::from_value(serde_json::json!({
            "type": "mongodb",
            "connection": { "url": { "type": "env", "variable": "MONGO_URL" } },
            "iam_auth": { "type": "aws_rds" }
        }))
        .unwrap();
        let DatabaseBranchConfig::Mongodb(cfg) = &branch else {
            panic!("expected a mongodb branch, got {branch:?}");
        };
        assert!(matches!(cfg.iam_auth, Some(IamAuthConfig::AwsRds { .. })));

        let gcp: DatabaseBranchConfig = serde_json::from_value(serde_json::json!({
            "type": "mongodb",
            "connection": { "url": { "type": "env", "variable": "MONGO_URL" } },
            "iam_auth": { "type": "gcp_cloud_sql" }
        }))
        .unwrap();
        let mut context = config::ConfigContext::default();
        let error = DatabaseBranchesConfig(vec![gcp])
            .verify(&mut context)
            .unwrap_err();
        assert!(error.to_string().contains("gcp_cloud_sql"), "{error}");
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

    /// The analytics counters report which copy modes, connection styles, and
    /// image/profile overrides branches use, not just their engines: a pg branch with
    /// default (empty) copy, params connection, a Secret-backed password, and a user
    /// image; a mysql branch with all-copy and a legacy URL connection; a remote redis
    /// branch with empty copy, a flat URL connection, and an admin profile; a generic
    /// branch whose mandatory image must not count as a user image override; and an s3
    /// branch, whose bucket is an extra param and which has no pod to override an image on.
    #[test]
    fn analytics_count_copy_mode_connection_and_image_traits() {
        let branches: Vec<DatabaseBranchConfig> = serde_json::from_str(
            r#"[
                {
                    "type": "pg",
                    "image": "registry.example.com/postgresql:15-partman",
                    "connection": {
                        "params": {
                            "host": "DB_HOST",
                            "port": "DB_PORT",
                            "password": { "secret": "rds-credentials", "key": "password" },
                            "database": "DB_NAME"
                        }
                    }
                },
                {
                    "type": "mysql",
                    "copy": { "mode": "all" },
                    "connection": { "url": { "type": "env", "variable": "DB_URL" } }
                },
                {
                    "type": "redis",
                    "profile": "telapp",
                    "connection": { "url": "REDIS_URL" }
                },
                {
                    "type": "generic",
                    "image": "docker.io/library/influxdb:2.7",
                    "port": 8086,
                    "connection": { "params": { "host": "INFLUX_HOST" } }
                },
                {
                    "type": "s3",
                    "copy": { "mode": "all" },
                    "source": { "params": { "bucket": "MY_BUCKET_ENV_VAR" } }
                }
            ]"#,
        )
        .unwrap();
        let config = DatabaseBranchesConfig(branches);

        let mut analytics = Analytics::default();
        (&config).collect_analytics(&mut analytics);

        assert_eq!(
            serde_json::to_value(&analytics).unwrap(),
            serde_json::json!({
                "clickhouse_branch_count": 0,
                "cockroachdb_branch_count": 0,
                "dynamodb_branch_count": 0,
                "generic_branch_count": 1,
                "mariadb_branch_count": 0,
                "mongodb_branch_count": 0,
                "mssql_branch_count": 0,
                "mysql_branch_count": 1,
                "pg_branch_count": 1,
                "redis_branch_count": 1,
                "s3_branch_count": 1,
                "spanner_branch_count": 0,
                "copy_empty_count": 2,
                "copy_schema_count": 0,
                "copy_all_count": 2,
                "connection_url_count": 2,
                "connection_params_count": 3,
                "connection_secret_count": 1,
                "params_host_count": 2,
                "params_port_count": 1,
                "params_user_count": 0,
                "params_password_count": 1,
                "params_database_count": 1,
                "params_extra_count": 1,
                "user_image_count": 1,
                "profile_count": 1,
            })
        );
    }

    fn base_with_ttl(ttl_secs: Option<u64>, ttl_mins: Option<u64>) -> BranchBaseConfig {
        BranchBaseConfig {
            ttl_secs,
            ttl_mins,
            ..Default::default()
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
        assert_eq!(base.resolved_ttl_secs(), BranchBaseConfig::DEFAULT_TTL_SECS);
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
        let mut pod = BranchPodConfig {
            version: None,
            image: Some("registry.example.com/postgresql:15-partman".to_owned()),
        };
        pod.verify().expect("image alone should verify");

        pod.version = Some("15".to_owned());
        assert!(matches!(pod.verify(), Err(ConfigError::Conflict(_))));
    }

    fn pg_branch_with_connection(connection: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Pg(Box::new(pg::PgBranchConfig {
            base: Default::default(),
            pod: Default::default(),
            database: DatabaseSourceConfig {
                name: None,
                connection,
            },
            copy: Default::default(),
            connection_settings: Default::default(),
            query_params: Default::default(),
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
            redis::LocalRedisBranchConfig {
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

    mod migrations {
        use super::*;

        fn database(name: Option<&str>) -> DatabaseSourceConfig {
            DatabaseSourceConfig {
                name: name.map(str::to_owned),
                connection: ConnectionSource::FlatUrl {
                    source_type: None,
                    url: "DB_URL".to_owned().into(),
                },
            }
        }

        fn parse(json: &str) -> SqlBranchMigrationsConfig {
            serde_json::from_str(json).unwrap()
        }

        /// The original local-directory form keeps parsing and verifying unchanged.
        #[test]
        fn flyway_local_path() {
            let config = parse(r#"{ "flavor": "flyway", "path": "./migrations" }"#);
            assert_eq!(
                config,
                SqlBranchMigrationsConfig::Flyway {
                    path: Some(PathBuf::from("./migrations")),
                    image: None,
                    locations: vec![],
                }
            );
            config.verify(&database(Some("db"))).unwrap();
        }

        /// Image-native Flyway: SQL baked into the job image, `locations` point inside it.
        #[test]
        fn flyway_in_image_locations() {
            let config = parse(
                r#"{
                    "flavor": "flyway",
                    "image": "example.com/migrations:1",
                    "locations": ["filesystem:/flyway/sql"]
                }"#,
            );
            assert_eq!(
                config,
                SqlBranchMigrationsConfig::Flyway {
                    path: None,
                    image: Some("example.com/migrations:1".to_owned()),
                    locations: vec!["filesystem:/flyway/sql".to_owned()],
                }
            );
            config.verify(&database(Some("db"))).unwrap();
        }

        /// A user-provided image and command run as the migration job (an app's own migration
        /// script).
        #[test]
        fn container_flavor() {
            let config = parse(
                r#"{
                    "flavor": "container",
                    "image": "example.com/app:1",
                    "command": ["./db_setup.sh"],
                    "env": { "SNAPSHOT_JOB": "true" }
                }"#,
            );
            assert_eq!(
                config,
                SqlBranchMigrationsConfig::Container {
                    image: "example.com/app:1".to_owned(),
                    command: Some(vec!["./db_setup.sh".to_owned()]),
                    args: None,
                    env: BTreeMap::from([("SNAPSHOT_JOB".to_owned(), "true".to_owned())]),
                }
            );
            config.verify(&database(Some("db"))).unwrap();
        }

        /// `path` uploads local files while `locations` reads from the image; the two sources
        /// cannot mix in one run.
        #[test]
        fn flyway_path_and_locations_conflict() {
            let config = parse(
                r#"{
                    "flavor": "flyway",
                    "path": "./migrations",
                    "image": "example.com/migrations:1",
                    "locations": ["filesystem:/flyway/sql"]
                }"#,
            );
            config.verify(&database(Some("db"))).unwrap_err();
        }

        /// Flyway with neither `path` nor `locations` has no migration files to run.
        #[test]
        fn flyway_without_files_rejected() {
            let config = parse(r#"{ "flavor": "flyway" }"#);
            config.verify(&database(Some("db"))).unwrap_err();
        }

        /// `locations` only make sense inside a user image, so they require `image`.
        #[test]
        fn flyway_locations_require_image() {
            let config =
                parse(r#"{ "flavor": "flyway", "locations": ["filesystem:/flyway/sql"] }"#);
            config.verify(&database(Some("db"))).unwrap_err();
        }

        /// Every flavor needs the branch `name` - the operator uses it as the target database.
        #[test]
        fn migrations_require_branch_name() {
            let config = parse(r#"{ "flavor": "container", "image": "example.com/app:1" }"#);
            config.verify(&database(None)).unwrap_err();
        }
    }
}
