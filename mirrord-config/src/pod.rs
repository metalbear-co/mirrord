use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::config::source::MirrordConfigSource;

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PodField {
    #[config(unwrap = true, env = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub name: Option<String>,

    #[config(
        unwrap = true,
        env = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE",
        default = "default"
    )]
    pub namespace: Option<String>,

    #[config(env = "MIRRORD_IMPERSONATED_CONTAINER_NAME")]
    pub container: Option<String>,
}
