use std::{borrow::Cow, collections::HashMap, fmt::Formatter};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use mirrord_config::feature::database_branches::{
    ConnectionParamsConfig, ConnectionSourceType, ParamSource, SingleOrVec,
    TargetEnvironmentVariableSource,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum_macros::EnumDiscriminants;

use crate::crd::session::SessionOwner;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSource {
    /// One or more complete connection URL sources.
    Url(SingleOrVec<ConnectionSourceKind>),
    /// Individual connection parameters (host, port, user, password, database).
    Params(Box<ConnectionParamsSpec>),
}

/// Individual connection parameters, each resolved from a separate environment variable.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionParamsSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<SingleOrVec<ConnectionSourceKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<SingleOrVec<ConnectionSourceKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<SingleOrVec<ConnectionSourceKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SingleOrVec<ConnectionSourceKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<SingleOrVec<ConnectionSourceKind>>,
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

    /// Value read directly from a Kubernetes Secret at resolution time.
    /// When `env_var_name` is set, the operator resolves the Secret and emits
    /// the value to the local mirrord process under that name. Without it,
    /// the Secret is only used by the operator for branch provisioning.
    Secret {
        name: String,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },

    /// Environment variable whose value is extracted via a regex pattern.
    EnvPattern {
        container: Option<String>,
        variable: String,
        value_pattern: String,
    },
}

impl From<TargetEnvironmentVariableSource> for ConnectionSourceKind {
    fn from(src: TargetEnvironmentVariableSource) -> Self {
        match src {
            TargetEnvironmentVariableSource::Env {
                container,
                variable,
                ..
            } => ConnectionSourceKind::Env {
                container,
                variable,
            },
            TargetEnvironmentVariableSource::EnvFrom {
                container,
                variable,
            } => ConnectionSourceKind::EnvFrom {
                container,
                variable,
            },
            TargetEnvironmentVariableSource::Secret {
                name,
                key,
                env_var_name,
            } => ConnectionSourceKind::Secret {
                name,
                key,
                env_var_name,
            },
        }
    }
}

impl From<&TargetEnvironmentVariableSource> for ConnectionSourceKind {
    fn from(src: &TargetEnvironmentVariableSource) -> Self {
        match src {
            TargetEnvironmentVariableSource::Env {
                container,
                variable,
                ..
            } => ConnectionSourceKind::Env {
                container: container.clone(),
                variable: variable.clone(),
            },
            TargetEnvironmentVariableSource::EnvFrom {
                container,
                variable,
            } => ConnectionSourceKind::EnvFrom {
                container: container.clone(),
                variable: variable.clone(),
            },
            TargetEnvironmentVariableSource::Secret {
                name,
                key,
                env_var_name,
            } => ConnectionSourceKind::Secret {
                name: name.clone(),
                key: key.clone(),
                env_var_name: env_var_name.clone(),
            },
        }
    }
}

impl From<&ConnectionParamsConfig> for ConnectionParamsSpec {
    fn from(config: &ConnectionParamsConfig) -> Self {
        let wrap =
            |params: &Option<SingleOrVec<ParamSource>>| -> Option<SingleOrVec<ConnectionSourceKind>> {
                params.as_ref().map(|one_or_many| {
                    let kinds: Vec<_> = one_or_many
                        .iter()
                        .map(|p| match p {
                            ParamSource::Variable(v) => match config.source_type.as_ref() {
                                Some(ConnectionSourceType::EnvFrom) => {
                                    ConnectionSourceKind::EnvFrom {
                                        container: None,
                                        variable: v.clone(),
                                    }
                                }
                                _ => ConnectionSourceKind::Env {
                                    container: None,
                                    variable: v.clone(),
                                },
                            },
                            ParamSource::Env { env_var_name, .. } => {
                                match config.source_type.as_ref() {
                                    Some(ConnectionSourceType::EnvFrom) => {
                                        ConnectionSourceKind::EnvFrom {
                                            container: None,
                                            variable: env_var_name.clone(),
                                        }
                                    }
                                    _ => ConnectionSourceKind::Env {
                                        container: None,
                                        variable: env_var_name.clone(),
                                    },
                                }
                            }
                            ParamSource::Pattern {
                                env_var_name,
                                value_pattern,
                            } => ConnectionSourceKind::EnvPattern {
                                container: None,
                                variable: env_var_name.clone(),
                                value_pattern: value_pattern.clone(),
                            },
                            ParamSource::Secret {
                                name,
                                key,
                                env_var_name,
                            } => ConnectionSourceKind::Secret {
                                name: name.clone(),
                                key: key.clone(),
                                env_var_name: env_var_name.clone(),
                            },
                        })
                        .collect();
                    SingleOrVec::from(kinds)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_name: Option<String>,
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
#[derive(Clone, Debug, Deserialize, Serialize, EnumDiscriminants)]
#[strum_discriminants(derive(Deserialize, Serialize, JsonSchema))]
#[strum_discriminants(serde(rename_all = "snake_case"))]
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

impl JsonSchema for IamAuthConfig {
    fn schema_name() -> Cow<'static, str> {
        "IamAuthConfig".into()
    }

    /// [`IamAuthConfig`] is adjacently tagged. Because of this, its JSON schema is not valid
    /// according to CRD standards.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        #[derive(Serialize, Deserialize, JsonSchema)]
        struct Proxy {
            // We verify the tag.
            #[serde(rename = "type")]
            tag: IamAuthConfigDiscriminants,
            // Other fields can be whatever.
            //
            // Generated CRD has `x-kubernetes-preserve-unknown-fields`,
            // so everything is all right.
            #[serde(flatten)]
            rest: HashMap<String, serde_json::Value>,
        }

        Proxy::json_schema(generator)
    }
}
