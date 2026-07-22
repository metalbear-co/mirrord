use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
};

use k8s_openapi::ByteString;
use kube::CustomResource;
use mirrord_config::feature::database_branches::{
    BranchItemCopyConfig, ClickhouseBranchCopyConfig, CockroachdbBranchCopyConfig,
    DynamodbBranchCopyConfig, MariadbBranchCopyConfig, MongodbBranchCopyConfig,
    MssqlBranchCopyConfig, MysqlBranchCopyConfig, PgBranchCopyConfig, PgIamAuthConfig,
    RedisBranchCopyConfig, SingleOrVec, SpannerBranchCopyConfig,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum_macros::EnumDiscriminants;

pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
use super::core::{ExtraParamSet, IamAuthConfig};
use crate::crd::session::SessionTarget;

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "BranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct BranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// Database name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: SessionTarget,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// Database server image version (e.g. "16" for PostgreSQL, "8.0" for MySQL).
    /// Mutually exclusive with `image`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Full image reference for the branch database container, including the tag.
    /// Overrides the operator-configured registry and the built-in default entirely; the
    /// operator validates it against the admin's per-database `allowedImages` list. Mutually
    /// exclusive with `version`. Generic branches carry their image in `genericOptions`
    /// instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// PostgreSQL-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_options: Option<PostgresOptions>,
    /// MySQL-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mysql_options: Option<MysqlOptions>,
    /// MariaDB-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mariadb_options: Option<MariadbOptions>,
    /// MongoDB-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mongodb_options: Option<MongodbOptions>,
    /// MSSQL-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mssql_options: Option<MssqlOptions>,
    /// Redis-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redis_options: Option<RedisOptions>,
    /// DynamoDB-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamodb_options: Option<DynamodbOptions>,
    /// Google Cloud Spanner-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spanner_options: Option<SpannerOptions>,
    /// ClickHouse-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickhouse_options: Option<ClickhouseOptions>,
    /// CockroachDB-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cockroachdb_options: Option<CockroachdbOptions>,
    /// Generic (user-supplied image) branch options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic_options: Option<GenericOptions>,
    /// Schema migrations to run on the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrations: Option<MigrationsSpec>,
}

/// Migrations to apply to a branch.
#[derive(Clone, Debug, Deserialize, Serialize, EnumDiscriminants)]
#[strum_discriminants(derive(Deserialize, Serialize, JsonSchema))]
#[strum_discriminants(serde(rename_all = "camelCase"))]
#[serde(tag = "flavor", rename_all = "camelCase")]
pub enum MigrationsSpec {
    Flyway {
        /// Overrides the container image used to run the migrations. Required with
        /// `locations`, which point inside this image.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        /// A gzipped tar of the migration files. Absent for image-native migrations, which
        /// carry their files inside `image` and select them with `locations`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive: Option<ByteString>,
        /// Flyway locations inside `image` holding the migration files
        /// (e.g. `filesystem:/flyway/sql`). Mutually exclusive with `archive`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locations: Vec<String>,
    },
    /// A user-provided image run as the migration job. The operator injects the branch
    /// connection as `MIRRORD_DB_HOST`/`PORT`/`USER`/`PASSWORD`/`NAME` env vars;
    /// `command`/`args`/`env` values can reference them with Kubernetes `$(VAR)` expansion.
    Container {
        /// Full image reference for the migration container, including the tag.
        image: String,
        /// Entrypoint command override for the migration container.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        /// Entrypoint args override for the migration container.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        /// Extra environment variables for the migration container.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
}

impl JsonSchema for MigrationsSpec {
    fn schema_name() -> Cow<'static, str> {
        "MigrationsSpec".into()
    }

    /// [`MigrationsSpec`] is internally tagged, and kube's structural-schema hoisting requires
    /// the tag property's schema to be identical across subschemas - which a multi-variant
    /// tagged enum can't satisfy. Like [`IamAuthConfig`](super::core::IamAuthConfig), the
    /// schema validates only the `flavor` tag and leaves the per-variant fields open
    /// (`x-kubernetes-preserve-unknown-fields`); the operator validates them on reconcile.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        #[derive(Serialize, Deserialize, JsonSchema)]
        struct Proxy {
            #[serde(rename = "flavor")]
            tag: MigrationsSpecDiscriminants,
            #[serde(flatten)]
            rest: HashMap<String, serde_json::Value>,
        }

        Proxy::json_schema(generator)
    }
}

/// Validated dialect configuration extracted from a [`BranchDatabaseSpec`].
/// Exactly one of the four option fields must be set; this enum represents
/// the result after that validation.
#[derive(Clone, Debug)]
pub enum DialectConfig {
    Postgres(Box<PostgresOptions>),
    Mysql(Box<MysqlOptions>),
    Mariadb(Box<MariadbOptions>),
    Dynamodb(Box<DynamodbOptions>),
    Mongodb(Box<MongodbOptions>),
    Mssql(Box<MssqlOptions>),
    Redis(Box<RedisOptions>),
    Spanner(Box<SpannerOptions>),
    Clickhouse(Box<ClickhouseOptions>),
    Cockroachdb(Box<CockroachdbOptions>),
    Generic(Box<GenericOptions>),
}

/// Simple discriminant enum for dialect matching without carrying option data.
/// Used by the operator controller to filter resources by database engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseDialect {
    Postgres,
    Mysql,
    Mariadb,
    Dynamodb,
    Mongodb,
    Mssql,
    Redis,
    Spanner,
    Clickhouse,
    Cockroachdb,
    Generic,
    #[serde(other)]
    Unknown,
}

impl DatabaseDialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Mariadb => "MariaDB",
            Self::Dynamodb => "DynamoDB",
            Self::Mongodb => "MongoDB",
            Self::Mssql => "MSSQL",
            Self::Redis => "Redis",
            Self::Spanner => "Spanner",
            Self::Clickhouse => "ClickHouse",
            Self::Cockroachdb => "CockroachDB",
            Self::Generic => "Generic",
            Self::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for DatabaseDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for DialectConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.dialect().fmt(f)
    }
}

impl DialectConfig {
    /// Extract the dialect discriminant (without options data).
    pub fn dialect(&self) -> DatabaseDialect {
        match self {
            Self::Postgres(_) => DatabaseDialect::Postgres,
            Self::Mysql(_) => DatabaseDialect::Mysql,
            Self::Mariadb(_) => DatabaseDialect::Mariadb,
            Self::Dynamodb(_) => DatabaseDialect::Dynamodb,
            Self::Mongodb(_) => DatabaseDialect::Mongodb,
            Self::Mssql(_) => DatabaseDialect::Mssql,
            Self::Redis(_) => DatabaseDialect::Redis,
            Self::Spanner(_) => DatabaseDialect::Spanner,
            Self::Clickhouse(_) => DatabaseDialect::Clickhouse,
            Self::Cockroachdb(_) => DatabaseDialect::Cockroachdb,
            Self::Generic(_) => DatabaseDialect::Generic,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DialectValidationError {
    #[error(
        "exactly one of postgresOptions, mysqlOptions, mariadbOptions, dynamodbOptions, mongodbOptions, mssqlOptions, redisOptions, spannerOptions, clickhouseOptions, cockroachdbOptions, or genericOptions must be set, but none were"
    )]
    NoneSet,
    #[error(
        "exactly one of postgresOptions, mysqlOptions, mariadbOptions, dynamodbOptions, mongodbOptions, mssqlOptions, redisOptions, spannerOptions, clickhouseOptions, or genericOptions must be set, but multiple were"
    )]
    MultipleSet,
    #[error("unknown connection param `{key}` for {dialect}; valid params: {valid}")]
    UnknownConnectionParam {
        dialect: &'static str,
        key: String,
        valid: String,
    },
}

/// PostgreSQL-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostgresOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
    /// IAM auth config for cloud-managed databases (RDS, Cloud SQL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
    /// Postgres settings (GUCs) applied to every source connection during the branch copy,
    /// sent via `PGOPTIONS`. Used for things an RLS policy reads (a tenant variable) or `role`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connection_settings: BTreeMap<String, String>,
}

/// MySQL-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MysqlOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
    /// IAM auth config for cloud-managed databases (RDS, Cloud SQL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

/// MariaDB-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MariadbOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
    /// IAM auth config for cloud-managed databases (RDS, Cloud SQL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

/// MySQL-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MssqlOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
}

/// ClickHouse-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClickhouseOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
}

/// CockroachDB-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CockroachdbOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
}

/// MongoDB-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbOptions {
    #[serde(default)]
    pub copy: MongodbCopySpec,
}

/// Redis-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedisOptions {
    #[serde(default)]
    pub copy: RedisCopySpec,
}

/// DynamoDB-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DynamodbOptions {
    #[serde(default)]
    pub copy: DynamodbCopySpec,
    /// IAM auth config used to read the source DynamoDB tables. DynamoDB has no
    /// password-based connection mode, so this is the only way to authenticate
    /// against the source for `all` copy mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

/// Generic (user-supplied image) branch options.
///
/// The branch runs the user's own image, starting empty - no copy, no engine knowledge.
/// The operator injects every resolved connection param into the branch container as a
/// `MIRRORD_PARAM_<NAME>` env var (Secret-backed params as `secretKeyRef`, never read by the
/// operator) so the container can bootstrap itself with the source's values via Kubernetes'
/// `$(VAR)` expansion in `command`/`args`/`env`. Only the app's `host`/`port` vars are
/// redirected to the branch; everything else is left untouched.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenericOptions {
    /// Full image reference for the branch container, including the tag.
    pub image: String,
    /// The port the branched service listens on. Used for the default readiness probe and as
    /// the port the app's connection is redirected to.
    pub port: u16,
    /// Entrypoint command override for the branch container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Entrypoint args override for the branch container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Extra environment variables for the branch container.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Readiness check for the branch container. Defaults to a TCP probe on `port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<GenericReadinessSpec>,
}

/// Readiness check for a generic branch container, shaped like a Kubernetes `Probe`: at most
/// one of `exec`/`httpGet` set (a tagged enum would break kube's structural-schema hoisting,
/// which requires the tag property's schema to be identical across subschemas). When neither
/// is set - or the whole `readiness` field is absent - the branch gets a TCP probe on `port`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenericReadinessSpec {
    /// Command probe; ready when the command exits 0 inside the branch container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<GenericExecProbeSpec>,
    /// HTTP GET probe; ready on a 2xx/3xx response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_get: Option<GenericHttpGetProbeSpec>,
}

/// Command readiness probe for a generic branch container.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenericExecProbeSpec {
    pub command: Vec<String>,
}

/// HTTP GET readiness probe for a generic branch container.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenericHttpGetProbeSpec {
    pub path: String,
    /// Defaults to the branch `port` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Google Cloud Spanner-specific branch options.
///
/// The branch runs the Cloud Spanner emulator; the app is redirected by injecting
/// `SPANNER_EMULATOR_HOST` so its `project`/`instance`/`database` values keep resolving,
/// now against the emulator. The app keeps those three values unchanged: the source
/// `project`/`instance`/`database` live in `connectionSource` `extra` keyed by [`SpannerParam`]
/// (never in a fixed slot, so no generic override rewrites them) and are resolved from the
/// target pod so the branch init sidecar can recreate and copy them.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpannerOptions {
    #[serde(default)]
    pub copy: SqlBranchCopyConfig,
    /// Name of the env var the operator sets on the local process to point it at the branch
    /// emulator. Defaults to `SPANNER_EMULATOR_HOST` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_host_var: Option<String>,
}

/// The extra connection params Spanner accepts, keyed into `ConnectionParamsSpec.extra`.
///
/// Single source of truth for the names: `from_spanner` builds the keys from these variants,
/// `BranchDatabaseSpec::dialect` validates the spec's keys against them, and
/// `spanner-branch-init` reads them back. Renaming a variant moves all three at once.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    strum_macros::Display,
    strum_macros::EnumString,
    strum_macros::EnumIter,
)]
#[strum(serialize_all = "camelCase")]
pub enum SpannerParam {
    Project,
    Instance,
    // Named `database_id`, not `database`, for two reasons:
    //  - Meaning: the fixed `database` slot is an override target - the operator rewrites the
    //    app's database var to the branch's database. Spanner's database is the opposite, a
    //    read-only source locator: the app keeps its own database and is redirected wholesale by
    //    SPANNER_EMULATOR_HOST, so the operator must never rewrite it.
    //  - Wire: the extra params are serialized flat next to the fixed slots, and serde binds a
    //    `database` key to the fixed slot rather than into `extra`, so the key has to differ.
    #[strum(serialize = "database_id")]
    Database,
}

impl ExtraParamSet for SpannerParam {
    fn parse(key: &str) -> Option<Self> {
        key.parse().ok()
    }

    fn valid_names() -> Vec<String> {
        <Self as strum::IntoEnumIterator>::iter()
            .map(|p| p.to_string())
            .collect()
    }
}

/// Read-only view of the common fields shared by all dialects.
pub struct CommonFieldsRef<'a> {
    pub id: &'a str,
    pub connection_source: &'a ConnectionSource,
    pub database_name: Option<&'a str>,
    pub target: &'a SessionTarget,
    pub ttl_secs: u64,
    pub version: Option<&'a str>,
    pub image: Option<&'a str>,
}

impl BranchDatabaseSpec {
    /// Validate and extract the dialect config from the spec.
    /// Exactly one dialect option field must be set.
    pub fn dialect(&self) -> Result<DialectConfig, DialectValidationError> {
        let mut dialects = [
            self.postgres_options
                .as_ref()
                .map(|v| DialectConfig::Postgres(Box::new(v.clone()))),
            self.mysql_options
                .as_ref()
                .map(|v| DialectConfig::Mysql(Box::new(v.clone()))),
            self.mariadb_options
                .as_ref()
                .map(|v| DialectConfig::Mariadb(Box::new(v.clone()))),
            self.dynamodb_options
                .as_ref()
                .map(|v| DialectConfig::Dynamodb(Box::new(v.clone()))),
            self.mongodb_options
                .as_ref()
                .map(|v| DialectConfig::Mongodb(Box::new(v.clone()))),
            self.mssql_options
                .as_ref()
                .map(|v| DialectConfig::Mssql(Box::new(v.clone()))),
            self.redis_options
                .as_ref()
                .map(|v| DialectConfig::Redis(Box::new(v.clone()))),
            self.spanner_options
                .as_ref()
                .map(|v| DialectConfig::Spanner(Box::new(v.clone()))),
            self.clickhouse_options
                .as_ref()
                .map(|v| DialectConfig::Clickhouse(Box::new(v.clone()))),
            self.cockroachdb_options
                .as_ref()
                .map(|v| DialectConfig::Cockroachdb(Box::new(v.clone()))),
            self.generic_options
                .as_ref()
                .map(|v| DialectConfig::Generic(Box::new(v.clone()))),
        ]
        .into_iter()
        .flatten();

        let config = dialects.next().ok_or(DialectValidationError::NoneSet)?;
        if dialects.next().is_some() {
            return Err(DialectValidationError::MultipleSet);
        }
        if let ConnectionSource::Params(params) = &self.connection_source {
            Self::validate_extra_params(&config, &params.extra)?;
        }
        Ok(config)
    }

    /// Reject `connection_source.extra` keys the resolved dialect does not define. Engines with
    /// an [`ExtraParamSet`] validate against it; engines without one accept no extra keys.
    fn validate_extra_params(
        config: &DialectConfig,
        extra: &BTreeMap<String, SingleOrVec<ConnectionSourceKind>>,
    ) -> Result<(), DialectValidationError> {
        fn check<P: ExtraParamSet>(
            dialect: &'static str,
            extra: &BTreeMap<String, SingleOrVec<ConnectionSourceKind>>,
        ) -> Result<(), DialectValidationError> {
            for key in extra.keys() {
                if P::parse(key).is_none() {
                    return Err(DialectValidationError::UnknownConnectionParam {
                        dialect,
                        key: key.clone(),
                        valid: P::valid_names().join(", "),
                    });
                }
            }
            Ok(())
        }

        match config {
            DialectConfig::Spanner(_) => check::<SpannerParam>("spanner", extra),
            // For generic branches the extras ARE the point: any key is accepted, as long as
            // it can become an env var name (`MIRRORD_PARAM_<KEY>`). Re-checked here because
            // CRDs can be created by non-CLI clients.
            DialectConfig::Generic(_) => {
                for key in extra.keys() {
                    let mut chars = key.chars();
                    let valid_key = chars
                        .next()
                        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
                    if !valid_key {
                        return Err(DialectValidationError::UnknownConnectionParam {
                            dialect: "generic",
                            key: key.clone(),
                            valid: "any key matching [A-Za-z_][A-Za-z0-9_]*".to_owned(),
                        });
                    }
                }
                Ok(())
            }
            other => match extra.keys().next() {
                Some(key) => Err(DialectValidationError::UnknownConnectionParam {
                    dialect: other.dialect().as_str(),
                    key: key.clone(),
                    valid: String::new(),
                }),
                None => Ok(()),
            },
        }
    }

    pub fn common(&self) -> CommonFieldsRef<'_> {
        CommonFieldsRef {
            id: &self.id,
            connection_source: &self.connection_source,
            database_name: self.database_name.as_deref(),
            target: &self.target,
            ttl_secs: self.ttl_secs,
            version: self.version.as_deref(),
            image: self.image.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SqlBranchCopyConfig {
    pub mode: SqlBranchCopyMode,
    /// Per-table copy filters. Only compatible with Empty and Schema modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
    /// Arguments passed to the database dump tool. When this field is set,
    /// it replaces the operator's dump defaults. An empty list means no dump args.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dump_args: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, strum_macros::AsRefStr)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum SqlBranchCopyMode {
    Empty,
    Schema,
    All,
}

impl Default for SqlBranchCopyConfig {
    fn default() -> Self {
        Self {
            mode: SqlBranchCopyMode::Empty,
            items: None,
            dump_args: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbCopySpec {
    pub mode: MongodbBranchCopyMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, strum_macros::AsRefStr)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum MongodbBranchCopyMode {
    Empty,
    All,
}

impl Default for MongodbCopySpec {
    fn default() -> Self {
        Self {
            mode: MongodbBranchCopyMode::Empty,
            items: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DynamodbCopySpec {
    pub mode: DynamodbBranchCopyMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, strum_macros::AsRefStr)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum DynamodbBranchCopyMode {
    Empty,
    All,
}

impl Default for DynamodbCopySpec {
    fn default() -> Self {
        Self {
            mode: DynamodbBranchCopyMode::Empty,
            items: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedisCopySpec {
    pub mode: RedisBranchCopyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, strum_macros::AsRefStr)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum RedisBranchCopyMode {
    Empty,
    All,
}

impl Default for RedisCopySpec {
    fn default() -> Self {
        Self {
            mode: RedisBranchCopyMode::Empty,
            patterns: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCopyConfig {
    /// Data that matches the filter will be copied.
    /// For SQL databases this is a WHERE clause (e.g. `username = 'alice'`).
    /// For MongoDB this is a JSON query document (e.g. `{"username": "alice"}`).
    pub filter: Option<String>,
}

impl From<&PgIamAuthConfig> for IamAuthConfig {
    fn from(config: &PgIamAuthConfig) -> Self {
        match config {
            PgIamAuthConfig::AwsRds {
                region,
                access_key_id,
                secret_access_key,
                session_token,
            } => IamAuthConfig::AwsRds {
                region: region.as_ref().map(Into::into),
                access_key_id: access_key_id.as_ref().map(Into::into),
                secret_access_key: secret_access_key.as_ref().map(Into::into),
                session_token: session_token.as_ref().map(Into::into),
            },
            PgIamAuthConfig::GcpCloudSql {
                credentials_json,
                credentials_path,
                project,
            } => IamAuthConfig::GcpCloudSql {
                credentials_json: credentials_json.as_ref().map(Into::into),
                credentials_path: credentials_path.as_ref().map(Into::into),
                project: project.as_ref().map(Into::into),
            },
        }
    }
}

impl From<PgBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: PgBranchCopyConfig) -> Self {
        match config {
            PgBranchCopyConfig::Empty { tables, dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args,
            },
            PgBranchCopyConfig::Schema { tables, dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args,
            },
            PgBranchCopyConfig::All { dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args,
            },
        }
    }
}

impl From<MysqlBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: MysqlBranchCopyConfig) -> Self {
        match config {
            MysqlBranchCopyConfig::Empty { tables, dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args,
            },
            MysqlBranchCopyConfig::Schema { tables, dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args,
            },
            MysqlBranchCopyConfig::All { dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args,
            },
        }
    }
}

impl From<MariadbBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: MariadbBranchCopyConfig) -> Self {
        match config {
            MariadbBranchCopyConfig::Empty { tables, dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args,
            },
            MariadbBranchCopyConfig::Schema { tables, dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args,
            },
            MariadbBranchCopyConfig::All { dump_args } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args,
            },
        }
    }
}

impl From<MssqlBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: MssqlBranchCopyConfig) -> Self {
        match config {
            MssqlBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            MssqlBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            MssqlBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args: None,
            },
        }
    }
}
impl From<CockroachdbBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: CockroachdbBranchCopyConfig) -> Self {
        match config {
            CockroachdbBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            CockroachdbBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            CockroachdbBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args: None,
            },
        }
    }
}
impl From<ClickhouseBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: ClickhouseBranchCopyConfig) -> Self {
        match config {
            ClickhouseBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            ClickhouseBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            ClickhouseBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args: None,
            },
        }
    }
}
impl From<SpannerBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: SpannerBranchCopyConfig) -> Self {
        match config {
            SpannerBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            SpannerBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
                dump_args: None,
            },
            SpannerBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
                dump_args: None,
            },
        }
    }
}

impl From<MongodbBranchCopyConfig> for MongodbCopySpec {
    fn from(config: MongodbBranchCopyConfig) -> Self {
        match config {
            MongodbBranchCopyConfig::Empty { collections } => MongodbCopySpec {
                mode: MongodbBranchCopyMode::Empty,
                items: convert_item_copy_configs(collections),
            },
            MongodbBranchCopyConfig::All { collections } => MongodbCopySpec {
                mode: MongodbBranchCopyMode::All,
                items: convert_item_copy_configs(collections),
            },
        }
    }
}

impl From<RedisBranchCopyConfig> for RedisCopySpec {
    fn from(config: RedisBranchCopyConfig) -> Self {
        match config {
            RedisBranchCopyConfig::Empty => RedisCopySpec {
                mode: RedisBranchCopyMode::Empty,
                patterns: None,
            },
            RedisBranchCopyConfig::All { patterns } => RedisCopySpec {
                mode: RedisBranchCopyMode::All,
                patterns: patterns.filter(|pattern| !pattern.is_empty()),
            },
        }
    }
}

impl From<DynamodbBranchCopyConfig> for DynamodbCopySpec {
    fn from(config: DynamodbBranchCopyConfig) -> Self {
        match config {
            DynamodbBranchCopyConfig::Empty { collections } => DynamodbCopySpec {
                mode: DynamodbBranchCopyMode::Empty,
                items: convert_item_copy_configs(collections),
            },
            DynamodbBranchCopyConfig::All { collections } => DynamodbCopySpec {
                mode: DynamodbBranchCopyMode::All,
                items: convert_item_copy_configs(collections),
            },
        }
    }
}

fn convert_item_copy_configs(
    items: Option<BTreeMap<String, BranchItemCopyConfig>>,
) -> Option<BTreeMap<String, ItemCopyConfig>> {
    items.map(|m| {
        m.into_iter()
            .map(|(name, config)| (name, ItemCopyConfig::from(config)))
            .collect()
    })
}

impl From<BranchItemCopyConfig> for ItemCopyConfig {
    fn from(config: BranchItemCopyConfig) -> Self {
        ItemCopyConfig {
            filter: config.filter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_copy_config_preserves_pg_dump_args() {
        let copy = SqlBranchCopyConfig::from(PgBranchCopyConfig::Schema {
            tables: None,
            dump_args: Some(vec!["--no-comments".to_owned()]),
        });

        assert!(matches!(copy.mode, SqlBranchCopyMode::Schema));
        assert!(copy.items.is_none());
        assert_eq!(copy.dump_args, Some(vec!["--no-comments".to_owned()]));
    }

    #[test]
    fn sql_copy_config_preserves_mysql_empty_dump_args() {
        let copy = SqlBranchCopyConfig::from(MysqlBranchCopyConfig::Empty {
            tables: Some(BTreeMap::from([(
                "users".to_owned(),
                BranchItemCopyConfig {
                    filter: Some("active = true".to_owned()),
                },
            )])),
            dump_args: Some(vec![]),
        });

        assert!(matches!(copy.mode, SqlBranchCopyMode::Empty));
        assert!(copy.items.is_some());
        assert_eq!(copy.dump_args, Some(vec![]));
    }

    /// Spanner's source locators parse from their flat wire keys, `database` is deliberately not a
    /// key (it would collide with the fixed slot), and the set drives validation error messages.
    #[test]
    fn spanner_param_parse_and_names() {
        assert_eq!(SpannerParam::parse("project"), Some(SpannerParam::Project));
        assert_eq!(
            SpannerParam::parse("instance"),
            Some(SpannerParam::Instance)
        );
        assert_eq!(
            SpannerParam::parse("database_id"),
            Some(SpannerParam::Database)
        );
        assert_eq!(SpannerParam::parse("database"), None);
        assert_eq!(SpannerParam::parse("nope"), None);

        let names = SpannerParam::valid_names();
        assert!(names.contains(&"database_id".to_owned()));
        assert!(!names.contains(&"database".to_owned()));
    }

    fn env_source(variable: &str) -> SingleOrVec<ConnectionSourceKind> {
        ConnectionSourceKind::Env {
            container: None,
            variable: variable.to_owned(),
        }
        .into()
    }

    #[test]
    fn validate_extra_params_spanner_accepts_known_and_rejects_unknown() {
        let config = DialectConfig::Spanner(Box::new(SpannerOptions {
            copy: SqlBranchCopyConfig::default(),
            emulator_host_var: None,
        }));

        let good = BTreeMap::from([
            ("project".to_owned(), env_source("GOOGLE_CLOUD_PROJECT")),
            ("instance".to_owned(), env_source("SPANNER_INSTANCE_ID")),
            ("database_id".to_owned(), env_source("SPANNER_DATABASE_ID")),
        ]);
        assert!(BranchDatabaseSpec::validate_extra_params(&config, &good).is_ok());

        let bad = BTreeMap::from([("database".to_owned(), env_source("X"))]);
        let err = BranchDatabaseSpec::validate_extra_params(&config, &bad).unwrap_err();
        assert!(matches!(
            err,
            DialectValidationError::UnknownConnectionParam {
                dialect: "spanner",
                ..
            }
        ));
    }
}
