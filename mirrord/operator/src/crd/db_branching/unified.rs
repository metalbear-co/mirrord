use std::collections::BTreeMap;

use kube::CustomResource;
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

/// MongoDB-specific branch options.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbOptions {
    #[serde(default)]
    pub copy: MongodbBranchCopyConfig,
}

// Manual JsonSchema because serde(flatten) on an externally-tagged enum produces
// allOf/oneOf nesting that Kubernetes rejects as non-structural. We build a flat
// object schema with oneOf enforcing exactly one dialect key.

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
        schema_gen
            .subschema_for::<k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta>(),
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
    let mongodb_schema = schema_gen.subschema_for::<MongodbOptions>();

    let mut properties = schemars::Map::new();
    properties.insert("id".into(), string_schema());
    properties.insert(
        "connectionSource".into(),
        schema_gen.subschema_for::<ConnectionSource>(),
    );
    properties.insert("databaseName".into(), string_schema());
    properties.insert(
        "target".into(),
        schema_gen.subschema_for::<SessionTarget>(),
    );
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

    let required: std::collections::BTreeSet<String> = [
        "id",
        "connectionSource",
        "target",
        "ttlSecs",
    ]
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

/// SQL copy config (PostgreSQL, MySQL). Supports Empty, Schema, and All modes
/// with optional per-table filters.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SqlBranchCopyConfig {
    pub mode: SqlBranchCopyMode,
    /// Per-table copy filters. Only compatible with Empty and Schema modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SqlBranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database with all tables' schema copied from the source database.
    Schema,
    /// Create a database and copy all tables' schema and data from the source database.
    /// With this copy mode, all item-specific copy configs are ignored.
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

/// Copy configuration for MongoDB.
/// Supports `Empty` and `All` copy modes with optional per-collection filters.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbBranchCopyConfig {
    /// The copy mode for the branch.
    pub mode: MongodbBranchCopyMode,

    /// An optional map of collections to copy with per-collection filter configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<BTreeMap<String, ItemCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MongodbBranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database and copy all collections and their data from the source database.
    /// Supports optional collection filters.
    All,
}

impl Default for MongodbBranchCopyConfig {
    fn default() -> Self {
        Self {
            mode: MongodbBranchCopyMode::Empty,
            items: None,
        }
    }
}

/// Per-item (table or collection) copy configuration.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCopyConfig {
    /// Data that matches the filter will be copied.
    /// For SQL databases this is a WHERE clause (e.g. `username = 'alice'`).
    /// For MongoDB this is a JSON query document (e.g. `{"username": "alice"}`).
    pub filter: Option<String>,
}
