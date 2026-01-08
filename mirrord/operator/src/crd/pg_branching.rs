use std::{
    collections::{BTreeMap, HashMap},
    fmt::Formatter,
};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use mirrord_config::{
    feature::database_branches::{
        ConnectionSourceKind as ConfigConnectionSourceKind, PgBranchCopyConfig,
        PgBranchTableCopyConfig, PgIamAuthConfig,
    },
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::session::SessionOwner;

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "PgBranchDatabase",
    status = "PgBranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct PgBranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// PostgreSQL database name.
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: Target,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// PostgreSQL server image version, e.g. "16".
    pub postgres_version: Option<String>,
    /// Options for copying data from source database to the branch.
    #[serde(default)]
    pub copy: BranchCopyConfig,
    /// IAM authentication configuration for the source database.
    /// Use this when the source database (RDS, Cloud SQL) requires IAM auth instead of passwords.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,
}

/// IAM authentication configuration for connecting to cloud-managed databases.
/// Environment variable sources follow the same pattern as `connection.url`:
/// - `Env` - direct env var from pod spec
/// - `EnvFrom` - from configMapRef/secretRef
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IamAuthConfig {
    /// AWS RDS/Aurora IAM authentication.
    /// Requires the init container to have AWS credentials
    AwsRds {
        /// AWS region.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<ConnectionSourceKind>,

        /// AWS Access Key ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access_key_id: Option<ConnectionSourceKind>,

        /// AWS Secret Access Key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_access_key: Option<ConnectionSourceKind>,

        /// AWS Session Token (for temporary credentials).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_token: Option<ConnectionSourceKind>,
    },
    /// GCP Cloud SQL IAM authentication.
    /// Requires the init container to have GCP credentials
    GcpCloudSql {
        /// Inline service account JSON key content.
        /// Specify the env var that contains the raw JSON content of the service account key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_json: Option<ConnectionSourceKind>,

        /// Path to service account JSON key file.
        /// Specify the env var that contains the file path to the service account key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_path: Option<ConnectionSourceKind>,

        /// GCP project ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ConnectionSourceKind>,
    },
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

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSource {
    /// A complete connection URL.
    Url(ConnectionSourceKind),
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSourceKind {
    /// Environment variable with value defined directly in the pod template.
    Env {
        container: Option<String>,
        variable: String,
    },

    /// Environment from resource reference in the the pod template.
    EnvFrom {
        container: Option<String>,
        variable: String,
    },
}

impl From<ConfigConnectionSourceKind> for ConnectionSourceKind {
    fn from(src: ConfigConnectionSourceKind) -> Self {
        match src {
            ConfigConnectionSourceKind::Env {
                container,
                variable,
            } => ConnectionSourceKind::Env {
                container,
                variable,
            },
            ConfigConnectionSourceKind::EnvFrom {
                container,
                variable,
            } => ConnectionSourceKind::EnvFrom {
                container,
                variable,
            },
        }
    }
}

impl From<&ConfigConnectionSourceKind> for ConnectionSourceKind {
    fn from(src: &ConfigConnectionSourceKind) -> Self {
        match src {
            ConfigConnectionSourceKind::Env {
                container,
                variable,
            } => ConnectionSourceKind::Env {
                container: container.clone(),
                variable: variable.clone(),
            },
            ConfigConnectionSourceKind::EnvFrom {
                container,
                variable,
            } => ConnectionSourceKind::EnvFrom {
                container: container.clone(),
                variable: variable.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PgBranchDatabaseStatus {
    pub phase: BranchDatabasePhase,
    /// Time when the branch database should be deleted.
    pub expire_time: MicroTime,
    /// Information of sessions that are using this branch database.
    #[serde(default)]
    pub session_info: HashMap<String, SessionInfo>,
    /// Error message when phase is Failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum BranchDatabasePhase {
    /// The controller is creating the branch database.
    Pending,
    /// The branch database is ready to use.
    Ready,
    /// The branch database creation failed.
    Failed,
}

impl std::fmt::Display for BranchDatabasePhase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabasePhase::Pending => write!(f, "Pending"),
            BranchDatabasePhase::Failed => write!(f, "Failed"),
            BranchDatabasePhase::Ready => write!(f, "Ready"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Owner info of the session.
    pub owner: SessionOwner,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCopyConfig {
    /// The default copy mode for the branch.
    pub mode: BranchCopyMode,

    /// An optional list of tables whose schema and data will be copied based on their
    /// table level copy config. Only compatible with `Empty` and `Schema` copy mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tables: Option<BTreeMap<String, TableCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database with all tables' schema copied from the source database.
    Schema,
    /// Create a database and copy all tables' schema and data from the source database.
    /// With this copy mode, all table specific copy configs are ignored.
    All,
}

impl Default for BranchCopyConfig {
    fn default() -> Self {
        BranchCopyConfig {
            mode: BranchCopyMode::Empty,
            tables: Default::default(),
        }
    }
}

impl From<PgBranchCopyConfig> for BranchCopyConfig {
    fn from(config: PgBranchCopyConfig) -> Self {
        match config {
            PgBranchCopyConfig::Empty { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                tables: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            PgBranchCopyConfig::Schema { tables } => BranchCopyConfig {
                mode: BranchCopyMode::Schema,
                tables: tables.map(|t| {
                    t.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            PgBranchCopyConfig::All => BranchCopyConfig {
                mode: BranchCopyMode::All,
                tables: None,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TableCopyConfig {
    /// Data that matches the filter will be copied.
    /// For PostgreSQL, this filter is a `WHERE` clause that looks like `username = 'alice'`.
    pub filter: Option<String>,
}

impl From<PgBranchTableCopyConfig> for TableCopyConfig {
    fn from(config: PgBranchTableCopyConfig) -> Self {
        TableCopyConfig {
            filter: config.filter,
        }
    }
}
