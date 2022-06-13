use envconfig::Envconfig;

#[derive(Envconfig, Clone)]
pub struct LayerConfig {
    #[envconfig(from = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub agent_rust_log: String,

    #[envconfig(from = "MIRRORD_AGENT_NAMESPACE", default = "default")]
    pub agent_namespace: String,

    #[envconfig(from = "MIRRORD_AGENT_IMAGE")]
    pub agent_image: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMAGE_PULL_POLICY", default = "IfNotPresent")]
    pub image_pull_policy: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub impersonated_pod_name: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", default = "default")]
    pub impersonated_pod_namespace: String,

    #[envconfig(from = "MIRRORD_ACCEPT_INVALID_CERTIFICATES", default = "false")]
    pub accept_invalid_certificates: bool,

    #[envconfig(from = "MIRRORD_AGENT_TTL", default = "0")]
    pub agent_ttl: u16,

    #[envconfig(from = "MIRRORD_FILE_OPS", default = "false")]
    pub enabled_file_ops: bool,

    /// Overrides local env variables with remote ones.
    #[envconfig(from = "MIRRORD_OVERRIDE_ENV_VARS", default = "false")]
    pub enabled_override_env_vars: bool,

    /// Filters out these env vars when overriding is enabled.
    #[envconfig(from = "MIRRORD_OVERRIDE_ENV_VARS_FILTER", default = "")]
    pub override_env_vars_filter: String,
}
