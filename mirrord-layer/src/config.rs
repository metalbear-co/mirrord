use envconfig::Envconfig;

pub mod env;
pub mod file;

#[derive(Clone)]
pub struct LayerConfig {
    pub agent_rust_log: String,
    pub agent_namespace: Option<String>,
    pub agent_image: Option<String>,
    pub image_pull_policy: String,
    pub impersonated_pod_name: String,
    pub impersonated_pod_namespace: String,
    pub impersonated_container_name: Option<String>,
    pub accept_invalid_certificates: bool,
    pub agent_ttl: u16,
    pub agent_tcp_steal_traffic: bool,
    pub agent_communication_timeout: Option<u16>,
    pub enabled_file_ops: bool,
    pub enabled_file_ro_ops: bool,
    pub override_env_vars_exclude: Option<String>,
    pub override_env_vars_include: Option<String>,
    pub ephemeral_container: bool,
    pub remote_dns: bool,
    pub enabled_tcp_outgoing: bool,
    pub enabled_udp_outgoing: bool,
}
