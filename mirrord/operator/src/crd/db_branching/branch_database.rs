use std::collections::BTreeMap;

use kube::CustomResource;
use mirrord_config::feature::database_branches::{
    BranchItemCopyConfig, MongodbBranchCopyConfig, MssqlBranchCopyConfig, MysqlBranchCopyConfig,
    PgBranchCopyConfig, PgIamAuthConfig,
};
use schemars::{
    JsonSchema,
    schema::{InstanceType, ObjectValidation, Schema, SchemaObject, SubschemaValidation},
};
use serde::{Deserialize, Serialize};

use super::core::IamAuthConfig;
pub use super::core::{
    BranchDatabasePhase, BranchDatabaseStatus, ConnectionSource, ConnectionSourceKind, SessionInfo,
};
use crate::crd::session::SessionTarget;

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "BranchDatabase",
    status = "BranchDatabaseStatus",
    namespaced,
    schema = "manual"
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
    /// Dialect-specific configuration. The key name determines the database engine.
    #[serde(flatten)]
    pub dialect: DialectConfig,
}

/// Externally-tagged enum where the key name (`postgresOptions`, `mysqlOptions`,
/// `mongodbOptions`) doubles as the dialect discriminant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DialectConfig {
    #[serde(rename = "postgresOptions")]
    Postgres(Box<PostgresOptions>),
    #[serde(rename = "mysqlOptions")]
    Mysql(Box<MysqlOptions>),
    #[serde(rename = "mongodbOptions")]
    Mongodb(Box<MongodbOptions>),
    #[serde(rename = "mssqlOptions")]
    Mssql(Box<MssqlOptions>),
}

/// Simple discriminant enum for dialect matching without carrying option data.
/// Used by the operator controller to filter resources by database engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseDialect {
    Postgres,
    Mysql,
    Mongodb,
    Mssql,
}

impl std::fmt::Display for DatabaseDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Postgres => write!(f, "PostgreSQL"),
            Self::Mysql => write!(f, "MySQL"),
            Self::Mongodb => write!(f, "MongoDB"),
            Self::Mssql => write!(f, "MSSQL"),
        }
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
            Self::Mongodb(_) => DatabaseDialect::Mongodb,
            Self::Mssql(_) => DatabaseDialect::Mssql,
        }
    }
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

impl JsonSchema for BranchDatabase {
    fn schema_name() -> String {
        "BranchDatabase".into()
    }

    fn json_schema(schema_gen: &mut schemars::r#gen::SchemaGenerator) -> Schema {
        crd_schema(schema_gen)
    }
}

fn crd_schema(schema_gen: &mut schemars::r#gen::SchemaGenerator) -> Schema {
    let spec_schema = spec_schema(schema_gen);
    let status_schema = schema_gen.subschema_for::<BranchDatabaseStatus>();

    let mut properties = schemars::Map::new();
    properties.insert("apiVersion".into(), string_schema());
    properties.insert("kind".into(), string_schema());
    properties.insert(
        "metadata".into(),
        schema_gen.subschema_for::<k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta>(),
    );
    properties.insert("spec".into(), spec_schema);
    properties.insert("status".into(), status_schema);

    SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        object: Some(Box::new(ObjectValidation {
            properties,
            required: ["apiVersion", "kind", "metadata", "spec"]
                .iter()
                .map(|s| (*s).into())
                .collect(),
            ..Default::default()
        })),
        ..Default::default()
    }
    .into()
}

fn spec_schema(schema_gen: &mut schemars::r#gen::SchemaGenerator) -> Schema {
    let pg_schema = schema_gen.subschema_for::<PostgresOptions>();
    let mysql_schema = schema_gen.subschema_for::<MysqlOptions>();
    let mongodb_schema = schema_gen.subschema_for::<MongodbCopySpec>();
    let mssql_schema = schema_gen.subschema_for::<MssqlOptions>();

    let mut properties = schemars::Map::new();
    properties.insert("id".into(), string_schema());
    properties.insert(
        "connectionSource".into(),
        schema_gen.subschema_for::<ConnectionSource>(),
    );
    properties.insert("databaseName".into(), string_schema());
    properties.insert("target".into(), schema_gen.subschema_for::<SessionTarget>());
    properties.insert(
        "ttlSecs".into(),
        SchemaObject {
            instance_type: Some(InstanceType::Integer.into()),
            format: Some("uint64".into()),
            ..Default::default()
        }
        .into(),
    );
    properties.insert("version".into(), string_schema());
    properties.insert("postgresOptions".into(), pg_schema);
    properties.insert("mysqlOptions".into(), mysql_schema);
    properties.insert("mongodbOptions".into(), mongodb_schema);
    properties.insert("mssqlOptions".into(), mssql_schema);

    let required: std::collections::BTreeSet<String> =
        ["id", "connectionSource", "target", "ttlSecs"]
            .iter()
            .map(|s| (*s).into())
            .collect();

    fn required_key(key: &str) -> Schema {
        SchemaObject {
            object: Some(Box::new(ObjectValidation {
                required: std::iter::once(key.to_owned()).collect(),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }

    SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        object: Some(Box::new(ObjectValidation {
            properties,
            required,
            ..Default::default()
        })),
        subschemas: Some(Box::new(SubschemaValidation {
            one_of: Some(vec![
                required_key("postgresOptions"),
                required_key("mysqlOptions"),
                required_key("mongodbOptions"),
                required_key("mssqlOptions"),
            ]),
            ..Default::default()
        })),
        ..Default::default()
    }
    .into()
}

fn string_schema() -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        ..Default::default()
    }
    .into()
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
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbCopySpec {
    pub mode: MongodbCopyMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, strum_macros::AsRefStr)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum MongodbCopyMode {
    Empty,
    All,
}

impl Default for MongodbCopySpec {
    fn default() -> Self {
        Self {
            mode: MongodbCopyMode::Empty,
            items: None,
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
            PgBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
            },
            PgBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
            },
            PgBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
            },
        }
    }
}

impl From<MysqlBranchCopyConfig> for SqlBranchCopyConfig {
    fn from(config: MysqlBranchCopyConfig) -> Self {
        match config {
            MysqlBranchCopyConfig::Empty { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Empty,
                items: convert_item_copy_configs(tables),
            },
            MysqlBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
            },
            MysqlBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
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
            },
            MssqlBranchCopyConfig::Schema { tables } => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::Schema,
                items: convert_item_copy_configs(tables),
            },
            MssqlBranchCopyConfig::All => SqlBranchCopyConfig {
                mode: SqlBranchCopyMode::All,
                items: None,
            },
        }
    }
}
impl From<MongodbBranchCopyConfig> for MongodbCopySpec {
    fn from(config: MongodbBranchCopyConfig) -> Self {
        match config {
            MongodbBranchCopyConfig::Empty { collections } => MongodbCopySpec {
                mode: MongodbCopyMode::Empty,
                items: convert_item_copy_configs(collections),
            },
            MongodbBranchCopyConfig::All { collections } => MongodbCopySpec {
                mode: MongodbCopyMode::All,
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
