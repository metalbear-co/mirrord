use std::collections::BTreeMap;

use kube::CustomResource;
use mirrord_config::feature::database_branches::{
    BranchItemCopyConfig, DynamodbBranchCopyConfig, MongodbBranchCopyConfig, MssqlBranchCopyConfig,
    MysqlBranchCopyConfig, PgBranchCopyConfig, PgIamAuthConfig, RedisBranchCopyConfig,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::core::IamAuthConfig;
pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// PostgreSQL-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_options: Option<PostgresOptions>,
    /// MySQL-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mysql_options: Option<MysqlOptions>,
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
}

/// Validated dialect configuration extracted from a [`BranchDatabaseSpec`].
/// Exactly one of the four option fields must be set; this enum represents
/// the result after that validation.
#[derive(Clone, Debug)]
pub enum DialectConfig {
    Postgres(Box<PostgresOptions>),
    Mysql(Box<MysqlOptions>),
    Dynamodb(Box<DynamodbOptions>),
    Mongodb(Box<MongodbOptions>),
    Mssql(Box<MssqlOptions>),
    Redis(Box<RedisOptions>),
}

/// Simple discriminant enum for dialect matching without carrying option data.
/// Used by the operator controller to filter resources by database engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseDialect {
    Postgres,
    Mysql,
    Dynamodb,
    Mongodb,
    Mssql,
    Redis,
    #[serde(other)]
    Unknown,
}

impl DatabaseDialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Dynamodb => "DynamoDB",
            Self::Mongodb => "MongoDB",
            Self::Mssql => "MSSQL",
            Self::Redis => "Redis",
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
            Self::Dynamodb(_) => DatabaseDialect::Dynamodb,
            Self::Mongodb(_) => DatabaseDialect::Mongodb,
            Self::Mssql(_) => DatabaseDialect::Mssql,
            Self::Redis(_) => DatabaseDialect::Redis,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DialectValidationError {
    #[error(
        "exactly one of postgresOptions, mysqlOptions, mongodbOptions, mssqlOptions, or redisOptions must be set, but none were"
    )]
    NoneSet,
    #[error(
        "exactly one of postgresOptions, mysqlOptions, mongodbOptions, mssqlOptions, or redisOptions must be set, but multiple were"
    )]
    MultipleSet,
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

/// MySQL-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MssqlOptions {
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
}

/// Read-only view of the common fields shared by all dialects.
pub struct CommonFieldsRef<'a> {
    pub id: &'a str,
    pub connection_source: &'a ConnectionSource,
    pub database_name: Option<&'a str>,
    pub target: &'a SessionTarget,
    pub ttl_secs: u64,
    pub version: Option<&'a str>,
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
            self.mongodb_options
                .as_ref()
                .map(|v| DialectConfig::Mongodb(Box::new(v.clone()))),
            self.mssql_options
                .as_ref()
                .map(|v| DialectConfig::Mssql(Box::new(v.clone()))),
            self.redis_options
                .as_ref()
                .map(|v| DialectConfig::Redis(Box::new(v.clone()))),
        ]
        .into_iter()
        .flatten();

        let config = dialects.next().ok_or(DialectValidationError::NoneSet)?;
        if dialects.next().is_some() {
            return Err(DialectValidationError::MultipleSet);
        }
        Ok(config)
    }

    pub fn common(&self) -> CommonFieldsRef<'_> {
        CommonFieldsRef {
            id: &self.id,
            connection_source: &self.connection_source,
            database_name: self.database_name.as_deref(),
            target: &self.target,
            ttl_secs: self.ttl_secs,
            version: self.version.as_deref(),
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
}
