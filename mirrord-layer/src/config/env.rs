use envconfig::Envconfig;

#[derive(Envconfig, Clone)]
pub struct LayerEnvConfig {
    #[envconfig(from = "MIRRORD_AGENT_RUST_LOG")]
    pub agent_rust_log: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_NAMESPACE")]
    pub agent_namespace: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMAGE")]
    pub agent_image: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMAGE_PULL_POLICY")]
    pub image_pull_policy: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub impersonated_pod_name: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE")]
    pub impersonated_pod_namespace: Option<String>,

    #[envconfig(from = "MIRRORD_IMPERSONATED_CONTAINER_NAME")]
    pub impersonated_container_name: Option<String>,

    #[envconfig(from = "MIRRORD_ACCEPT_INVALID_CERTIFICATES")]
    pub accept_invalid_certificates: Option<bool>,

    #[envconfig(from = "MIRRORD_AGENT_TTL")]
    pub agent_ttl: Option<u16>,

    #[envconfig(from = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC")]
    pub agent_tcp_steal_traffic: Option<bool>,

    #[envconfig(from = "MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    pub agent_communication_timeout: Option<u16>,

    #[envconfig(from = "MIRRORD_FILE_OPS")]
    pub enabled_file_ops: Option<bool>,

    #[envconfig(from = "MIRRORD_FILE_RO_OPS")]
    pub enabled_file_ro_ops: Option<bool>,

    #[envconfig(from = "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")]
    pub override_env_vars_exclude: Option<String>,

    #[envconfig(from = "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")]
    pub override_env_vars_include: Option<String>,

    #[envconfig(from = "MIRRORD_EPHEMERAL_CONTAINER")]
    pub ephemeral_container: Option<bool>,

    #[envconfig(from = "MIRRORD_REMOTE_DNS")]
    pub remote_dns: Option<bool>,

    #[envconfig(from = "MIRRORD_TCP_OUTGOING")]
    pub enabled_tcp_outgoing: Option<bool>,

    #[envconfig(from = "MIRRORD_UDP_OUTGOING")]
    pub enabled_udp_outgoing: Option<bool>,
}
