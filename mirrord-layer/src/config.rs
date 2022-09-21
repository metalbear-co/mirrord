use std::str::FromStr;

use envconfig::Envconfig;

#[derive(Envconfig, Debug, Clone)]
pub struct LayerConfig {
    #[envconfig(from = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub agent_rust_log: String,

    #[envconfig(from = "MIRRORD_AGENT_NAMESPACE")]
    pub agent_namespace: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMAGE")]
    pub agent_image: Option<String>,

    #[envconfig(from = "MIRRORD_AGENT_IMAGE_PULL_POLICY", default = "IfNotPresent")]
    pub image_pull_policy: String,

    #[envconfig(from = "MIRRORD_TARGET")]
    pub target: String,

    #[envconfig(from = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", default = "default")]
    pub impersonated_pod_namespace: String,

    #[envconfig(from = "MIRRORD_ACCEPT_INVALID_CERTIFICATES", default = "false")]
    pub accept_invalid_certificates: bool,

    #[envconfig(from = "MIRRORD_AGENT_TTL", default = "0")]
    pub agent_ttl: u16,

    #[envconfig(from = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default = "false")]
    pub agent_tcp_steal_traffic: bool,

    #[envconfig(from = "MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    pub agent_communication_timeout: Option<u16>,

    /// Enable file ops read/write
    #[envconfig(from = "MIRRORD_FILE_OPS", default = "false")]
    pub enabled_file_ops: bool,

    /// Enable file ops ready only (write will happen locally)
    #[envconfig(from = "MIRRORD_FILE_RO_OPS", default = "true")]
    pub enabled_file_ro_ops: bool,

    /// Filters out these env vars when overriding is enabled.
    #[envconfig(from = "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")]
    pub override_env_vars_exclude: Option<String>,

    /// Selects these env vars when overriding is enabled.
    #[envconfig(from = "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")]
    pub override_env_vars_include: Option<String>,

    #[envconfig(from = "MIRRORD_EPHEMERAL_CONTAINER", default = "false")]
    pub ephemeral_container: bool,

    /// Enables resolving a remote DNS.
    #[envconfig(from = "MIRRORD_REMOTE_DNS", default = "true")]
    pub remote_dns: bool,

    #[envconfig(from = "MIRRORD_TCP_OUTGOING", default = "true")]
    pub enabled_tcp_outgoing: bool,

    #[envconfig(from = "MIRRORD_UDP_OUTGOING", default = "true")]
    pub enabled_udp_outgoing: bool,

    #[envconfig(from = "MIRRORD_SKIP_PROCESSES")]
    pub skip_processes: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Deployment(pub String);

#[derive(Debug, PartialEq)]
pub enum Target {
    Deployment(Deployment),
    Pod(PodAndContainer),
}

impl FromStr for Deployment {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split = s.split('/').collect::<Vec<&str>>();
        match split.first() {
            Some(&"deployment") => Ok(Deployment(split[1].to_string())),
            _ => Err(format!("Given target: {:?} is not a deployment.", s)),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct PodAndContainer(pub String, pub Option<String>);

impl FromStr for PodAndContainer {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split = s.split('/').collect::<Vec<&str>>();
        match split.first() {
            Some(&"pod") if split.len() == 2 => Ok(Pod(split[1].to_string(), None)),
            Some(&"pod") if split.len() == 4 && split[2] == "container" => {
                Ok(Pod(split[1].to_string(), Some(split[3].to_string())))
            }
            _ => Err(format!("Given target: {:?} is not a pod.", s)),
        }
    }
}
