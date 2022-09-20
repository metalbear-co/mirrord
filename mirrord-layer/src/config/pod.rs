use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::config::util::{ConfigError, MirrordConfig};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PodField {
    #[unwrap_option]
    #[from_env("MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub name: Option<String>,

    #[default_value("default")]
    #[from_env("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE")]
    pub namespace: Option<String>,

    #[from_env("MIRRORD_IMPERSONATED_CONTAINER_NAME")]
    pub container: Option<String>,
}
