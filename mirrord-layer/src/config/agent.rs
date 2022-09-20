use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::config::util::{ConfigError, MirrordConfig};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct AgentField {
    #[default_value("info")]
    #[from_env("MIRRORD_AGENT_RUST_LOG")]
    pub log_level: Option<String>,

    #[from_env("MIRRORD_AGENT_NAMESPACE")]
    pub namespace: Option<String>,

    #[from_env("MIRRORD_AGENT_IMAGE")]
    pub image: Option<String>,

    #[default_value("IfNotPresent")]
    #[from_env("MIRRORD_AGENT_IMAGE_PULL_POLICY")]
    pub image_pull_policy: Option<String>,

    #[default_value("0")]
    #[from_env("MIRRORD_AGENT_TTL")]
    pub ttl: Option<u16>,

    #[default_value("false")]
    #[from_env("MIRRORD_EPHEMERAL_CONTAINER")]
    pub ephemeral: Option<bool>,

    #[from_env("MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    pub communication_timeout: Option<u16>,
}
