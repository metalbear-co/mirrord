use std::{collections::HashMap, fmt::Formatter};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use mirrord_config::feature::database_branches::{
    ConnectionParamsConfig, ConnectionSourceType, TargetEnviromentVariableSource,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::session::SessionOwner;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSource {
    /// A complete connection URL.
    Url(ConnectionSourceKind),
    /// Individual connection parameters (host, port, user, password, database).
    Params(ConnectionParamsSpec),
}

/// Individual connection parameters, each resolved from a separate environment variable.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionParamsSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<ConnectionSourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<ConnectionSourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<ConnectionSourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<ConnectionSourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<ConnectionSourceKind>,
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

impl From<TargetEnviromentVariableSource> for ConnectionSourceKind {
    fn from(src: TargetEnviromentVariableSource) -> Self {
        match src {
            TargetEnviromentVariableSource::Env {
                container,
                variable,
            } => ConnectionSourceKind::Env {
                container,
                variable,
            },
            TargetEnviromentVariableSource::EnvFrom {
                container,
                variable,
            } => ConnectionSourceKind::EnvFrom {
                container,
                variable,
            },
        }
    }
}

impl From<&TargetEnviromentVariableSource> for ConnectionSourceKind {
    fn from(src: &TargetEnviromentVariableSource) -> Self {
        match src {
            TargetEnviromentVariableSource::Env {
                container,
                variable,
            } => ConnectionSourceKind::Env {
                container: container.clone(),
                variable: variable.clone(),
            },
            TargetEnviromentVariableSource::EnvFrom {
                container,
                variable,
            } => ConnectionSourceKind::EnvFrom {
                container: container.clone(),
                variable: variable.clone(),
            },
        }
    }
}

impl From<&ConnectionParamsConfig> for ConnectionParamsSpec {
    fn from(config: &ConnectionParamsConfig) -> Self {
        let wrap = |var: &Option<String>| -> Option<ConnectionSourceKind> {
            var.as_ref().map(|v| match &config.source_type {
                ConnectionSourceType::Env => ConnectionSourceKind::Env {
                    container: None,
                    variable: v.clone(),
                },
                ConnectionSourceType::EnvFrom => ConnectionSourceKind::EnvFrom {
                    container: None,
                    variable: v.clone(),
                },
            })
        };
        Self {
            host: wrap(&config.params.host),
            port: wrap(&config.params.port),
            user: wrap(&config.params.user),
            password: wrap(&config.params.password),
            database: wrap(&config.params.database),
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

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum BranchDatabasePhase {
    /// Initial phase set by CLI. Controller will create the database pod.
    Init,
    /// The controller is creating the branch database pod.
    Pending,
    /// The branch database is ready to use.
    Ready,
    /// The branch database creation failed.
    Failed,
}

impl std::fmt::Display for BranchDatabasePhase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabasePhase::Init => write!(f, "Init"),
            BranchDatabasePhase::Pending => write!(f, "Pending"),
            BranchDatabasePhase::Ready => write!(f, "Ready"),
            BranchDatabasePhase::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchDatabaseStatus {
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
