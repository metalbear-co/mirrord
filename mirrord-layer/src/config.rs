use envconfig::Envconfig;

#[derive(Envconfig)]
pub struct Config {
    #[envconfig(from = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub agent_rust_log: String,

    #[envconfig(from = "MIRRORD_AGENT_NAMESPACE", default = "default")]
    pub agent_namespace: String,

    #[envconfig(
        from = "MIRRORD_AGENT_IMAGE",
        default = "ghcr.io/metalbear-co/mirrord-agent:2.0.0-alpha-3"
    )]
    pub agent_image: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub impersonated_pod_name: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", default = "default")]
    pub impersonated_pod_namespace: String,

    #[envconfig(from = "MIRRORD_ACCEPT_INVALID_CERTIFICATES", default = "false")]
    pub accept_invalid_certificates: bool,
}
