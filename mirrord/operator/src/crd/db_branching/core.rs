use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fmt::Formatter,
};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, MicroTime};
use mirrord_config::feature::database_branches::{
    ConfigMapRef, ConnectionParamsConfig, ConnectionSourceType, ParamSource, SingleOrVec,
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
    /// Engine-specific connection params keyed by name (e.g. Spanner `project` / `instance`).
    /// Resolved through the same source machinery as the fixed slots; each entry becomes a
    /// `--src-db-param <name>=<ref>` arg for the init binary. The accepted key set is owned by
    /// the engine (see `ExtraParamSet`) and validated per dialect, so unknown keys are rejected.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, SingleOrVec<ConnectionSourceKind>>,
}

/// The set of engine-specific connection param names an engine accepts.
///
/// Implemented by a per-engine enum (e.g. `SpannerParam`) so the enum is the single source of
/// truth: it builds the keys on the client, validates them on the operator, and is read back in
/// the init binary. Keeps `ConnectionParamsSpec.extra` generic while making its keys typed.
pub trait ExtraParamSet: Sized {
    /// Parse a wire key into a known param, or `None` if the key is not recognized.
    fn parse(key: &str) -> Option<Self>;

    /// Every accepted param name, used to build helpful validation errors.
    fn valid_names() -> &'static [&'static str];
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

    /// Value fetched from Google Secret Manager by the branch init container at
    /// data-copy time, using the target pod's service account (Workload Identity).
    /// The operator never reads the secret; it only passes `secret_ref` to the
    /// init container. When `env_var_name` is set, the local mirrord process gets
    /// the branch DB connection detail under that name (same semantics as `Secret`).
    GcpSecretManager {
        secret_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },

    /// Value fetched from AWS Secrets Manager by the branch init container at
    /// data-copy time, using the target pod's service account (IRSA / EKS Pod
    /// Identity). `secret_ref` is a secret name or full ARN, passed verbatim to
    /// `GetSecretValue`. Same semantics as `GcpSecretManager` otherwise.
    AwsSecretsManager {
        secret_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },

    /// Value read by the operator from a Kubernetes ConfigMap in the target namespace at
    /// resolution time. `key` picks the data entry; `value_selector` (a `.a.b` selector over
    /// the entry parsed as JSON/YAML) or `value_pattern` (a regex over the raw text) narrows
    /// it down to the param, with the whole entry used when neither is set. Setting both is
    /// rejected at resolution. When `env_var_name` is set, the local mirrord process gets the
    /// branch DB connection detail under that name (same semantics as `Secret`).
    ///
    /// `config_map` and `key` are each optional: whichever is missing comes from the
    /// `sourceConfigMap` default of the branch's admin profile, and a hole in both is a
    /// resolution error.
    ConfigMap {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_map: Option<ConfigMapLocator>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_var_name: Option<String>,
    },
}

/// Where a `ConfigMap` connection source finds its ConfigMap.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConfigMapLocator {
    /// The ConfigMap's object name.
    Name(String),
    /// The name of a `configMap` volume in the target pod's `spec.volumes`; the ConfigMap is
    /// the one that volume references, and `key` is resolved through the volume's `items`
    /// remapping when present.
    Volume(String),
}

impl From<&ConfigMapRef> for ConfigMapLocator {
    fn from(config_map: &ConfigMapRef) -> Self {
        match config_map {
            ConfigMapRef::Name(name) => Self::Name(name.clone()),
            ConfigMapRef::Volume { volume } => Self::Volume(volume.clone()),
        }
    }
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
            TargetEnvironmentVariableSource::GcpSecretManager {
                secret_ref,
                env_var_name,
            } => ConnectionSourceKind::GcpSecretManager {
                secret_ref,
                env_var_name,
            },
            TargetEnvironmentVariableSource::AwsSecretsManager {
                secret_ref,
                env_var_name,
            } => ConnectionSourceKind::AwsSecretsManager {
                secret_ref,
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
            TargetEnvironmentVariableSource::GcpSecretManager {
                secret_ref,
                env_var_name,
            } => ConnectionSourceKind::GcpSecretManager {
                secret_ref: secret_ref.clone(),
                env_var_name: env_var_name.clone(),
            },
            TargetEnvironmentVariableSource::AwsSecretsManager {
                secret_ref,
                env_var_name,
            } => ConnectionSourceKind::AwsSecretsManager {
                secret_ref: secret_ref.clone(),
                env_var_name: env_var_name.clone(),
            },
        }
    }
}

/// Map one client-config [`ParamSource`] to the CRD [`ConnectionSourceKind`], honoring the
/// connection's `source_type` (env vs envFrom) for plain variable references.
pub fn param_source_to_kind(
    param: &ParamSource,
    source_type: Option<&ConnectionSourceType>,
) -> ConnectionSourceKind {
    let as_env = |variable: String| match source_type {
        Some(ConnectionSourceType::EnvFrom) => ConnectionSourceKind::EnvFrom {
            container: None,
            variable,
        },
        _ => ConnectionSourceKind::Env {
            container: None,
            variable,
        },
    };
    match param {
        ParamSource::Variable(v) => as_env(v.clone()),
        ParamSource::Env { env_var_name, .. } => as_env(env_var_name.clone()),
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
        ParamSource::GcpSecretManager {
            secret_ref,
            env_var_name,
        } => ConnectionSourceKind::GcpSecretManager {
            secret_ref: secret_ref.clone(),
            env_var_name: env_var_name.clone(),
        },
        ParamSource::AwsSecretsManager {
            secret_ref,
            env_var_name,
        } => ConnectionSourceKind::AwsSecretsManager {
            secret_ref: secret_ref.clone(),
            env_var_name: env_var_name.clone(),
        },
        ParamSource::ConfigMap {
            config_map,
            key,
            value_selector,
            value_pattern,
            env_var_name,
        } => ConnectionSourceKind::ConfigMap {
            config_map: config_map.as_ref().map(ConfigMapLocator::from),
            key: key.clone(),
            value_selector: value_selector.clone(),
            value_pattern: value_pattern.clone(),
            env_var_name: env_var_name.clone(),
        },
    }
}

impl From<&ConnectionParamsConfig> for ConnectionParamsSpec {
    fn from(config: &ConnectionParamsConfig) -> Self {
        let wrap =
            |params: &Option<SingleOrVec<ParamSource>>| -> Option<SingleOrVec<ConnectionSourceKind>> {
                params.as_ref().map(|one_or_many| {
                    let kinds: Vec<_> = one_or_many
                        .iter()
                        .map(|p| param_source_to_kind(p, config.source_type.as_ref()))
                        .collect();
                    SingleOrVec::from(kinds)
                })
            };
        let extra = config
            .params
            .extra
            .iter()
            .map(|(name, sources)| {
                let kinds: Vec<_> = sources
                    .iter()
                    .map(|p| param_source_to_kind(p, config.source_type.as_ref()))
                    .collect();
                (name.clone(), SingleOrVec::from(kinds))
            })
            .collect();
        Self {
            host: wrap(&config.params.host),
            port: wrap(&config.params.port),
            user: wrap(&config.params.user),
            password: wrap(&config.params.password),
            database: wrap(&config.params.database),
            extra,
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
    /// A phase this build does not recognise.
    #[schemars(skip)]
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for BranchDatabasePhase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabasePhase::Init => write!(f, "Init"),
            BranchDatabasePhase::Pending => write!(f, "Pending"),
            BranchDatabasePhase::Ready => write!(f, "Ready"),
            BranchDatabasePhase::Unknown => write!(f, "Unknown"),
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
    /// Outcome of the branch's schema migrations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrations: Option<MigrationRun>,
    /// Outcome of a generic branch's copy Job. Unlike migrations, the copy runs at most once
    /// per branch (only while the branch is still Pending), so this never resets on
    /// generation bumps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<MigrationRun>,
    /// Standard conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

/// Outcome of running a branch's migrations.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationRun {
    /// The `metadata.generation` this run reconciled.
    pub observed_generation: i64,
    /// Phase of the migration run.
    pub phase: MigrationPhase,
    /// The migration tool's error output when `phase` is `Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Phase of a branch's migration run.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum MigrationPhase {
    Running,
    Succeeded,
    Failed,
    /// A phase this build does not recognise.
    #[schemars(skip)]
    #[serde(other)]
    Unknown,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The client-side ConfigMap source maps onto the CRD kind field by field, with the
    /// locator form preserved: the operator needs to know whether to look the ConfigMap up
    /// by name or through the pod's volume.
    #[test]
    fn configmap_param_source_maps_to_crd_kind() {
        let source = ParamSource::ConfigMap {
            config_map: Some(ConfigMapRef::Volume {
                volume: "app-config".to_owned(),
            }),
            key: Some("config.yml".to_owned()),
            value_selector: Some(".database.host".to_owned()),
            value_pattern: None,
            env_var_name: Some("DB_HOST".to_owned()),
        };
        let ConnectionSourceKind::ConfigMap {
            config_map,
            key,
            value_selector,
            value_pattern,
            env_var_name,
        } = param_source_to_kind(&source, None)
        else {
            panic!("expected a ConfigMap kind");
        };
        assert!(matches!(config_map, Some(ConfigMapLocator::Volume(v)) if v == "app-config"));
        assert_eq!(key.as_deref(), Some("config.yml"));
        assert_eq!(value_selector.as_deref(), Some(".database.host"));
        assert_eq!(value_pattern, None);
        assert_eq!(env_var_name.as_deref(), Some("DB_HOST"));

        let by_name = ParamSource::ConfigMap {
            config_map: Some(ConfigMapRef::Name("app-config".to_owned())),
            key: Some("config.yml".to_owned()),
            value_selector: None,
            value_pattern: None,
            env_var_name: None,
        };
        assert!(matches!(
            param_source_to_kind(&by_name, None),
            ConnectionSourceKind::ConfigMap {
                config_map: Some(ConfigMapLocator::Name(name)),
                ..
            } if name == "app-config"
        ));

        // Profile-backed: nothing to locate the ConfigMap with travels on the CR; the operator
        // fills it from the profile.
        let from_profile = ParamSource::ConfigMap {
            config_map: None,
            key: None,
            value_selector: Some(".database.host".to_owned()),
            value_pattern: None,
            env_var_name: Some("DB_HOST".to_owned()),
        };
        assert!(matches!(
            param_source_to_kind(&from_profile, None),
            ConnectionSourceKind::ConfigMap {
                config_map: None,
                key: None,
                ..
            }
        ));
    }

    #[test]
    fn unknown_migration_phase_deserializes_as_unknown() {
        let phase: MigrationPhase = serde_json::from_value(serde_json::json!("SomeFuturePhase"))
            .expect("unknown phases should fall back to Unknown");
        assert_eq!(phase, MigrationPhase::Unknown);
    }

    /// The client config's flat engine-specific keys survive the conversion into the CRD spec:
    /// fixed slots keep their own resolution, and every `extra` key is carried across.
    #[test]
    fn connection_params_spec_from_config_propagates_extra() {
        let json = r#"{
            "params": {
                "database": "DB_NAME",
                "project": "GOOGLE_CLOUD_PROJECT",
                "instance": "SPANNER_INSTANCE_ID",
                "database_id": "SPANNER_DATABASE_ID"
            }
        }"#;
        let config: ConnectionParamsConfig = serde_json::from_str(json).unwrap();
        let spec = ConnectionParamsSpec::from(&config);

        assert!(matches!(
            spec.database.as_ref().and_then(|s| s.iter().next()),
            Some(ConnectionSourceKind::Env { variable, .. }) if variable == "DB_NAME"
        ));

        let mut extra_keys: Vec<_> = spec.extra.keys().cloned().collect();
        extra_keys.sort();
        assert_eq!(extra_keys, ["database_id", "instance", "project"]);
        assert!(!spec.extra.contains_key("database"));

        assert!(matches!(
            spec.extra.get("database_id").and_then(|s| s.iter().next()),
            Some(ConnectionSourceKind::Env { variable, .. }) if variable == "SPANNER_DATABASE_ID"
        ));
    }
}
