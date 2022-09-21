use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::config::source::MirrordConfigSource;

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct AgentField {
    #[config(env = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub log_level: Option<String>,

    #[config(env = "MIRRORD_AGENT_NAMESPACE")]
    pub namespace: Option<String>,

    #[config(env = "MIRRORD_AGENT_IMAGE")]
    pub image: Option<String>,

    #[config(env = "MIRRORD_AGENT_IMAGE_PULL_POLICY", default = "IfNotPresent")]
    pub image_pull_policy: Option<String>,

    #[config(env = "MIRRORD_AGENT_TTL", default = "0")]
    pub ttl: Option<u16>,

    #[config(env = "MIRRORD_EPHEMERAL_CONTAINER", default = "false")]
    pub ephemeral: Option<bool>,

    #[config(env = "MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    pub communication_timeout: Option<u16>,
}
