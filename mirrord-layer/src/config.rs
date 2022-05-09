use envconfig::Envconfig;

#[derive(Envconfig)]
pub struct Config {
    #[envconfig(from = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub agent_rust_log: String,

    #[envconfig(from = "MIRRORD_AGENT_NAMESPACE", default = "default")]
    pub agent_namespace: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub impersonated_pod_name: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", default = "default")]
    pub impersonated_pod_namespace: String,
}
