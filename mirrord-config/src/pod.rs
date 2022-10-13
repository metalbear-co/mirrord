// START | To be removed after deprecated functionality is removed
use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::config::source::MirrordConfigSource;

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = PodConfig)]
pub struct PodFileConfig {
    #[config(env = "MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    pub name: Option<String>,

    #[config(env = "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE")]
    pub namespace: Option<String>,

    #[config(env = "MIRRORD_IMPERSONATED_CONTAINER_NAME")]
    pub container: Option<String>,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((Some("pod"), Some("pod".to_string())))] name: (Option<&str>, Option<String>),
        #[values((None, None), (Some("namespace"), Some("namespace".to_string())))] namespace: (
            Option<&str>,
            Option<String>,
        ),
        #[values((None, None), (Some("container"), Some("container")))] container: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_AGENT_IMPERSONATED_POD_NAME", name.0),
                ("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", namespace.0),
                ("MIRRORD_IMPERSONATED_CONTAINER_NAME", container.0),
            ],
            || {
                let outgoing = PodFileConfig::default().generate_config().unwrap();

                assert_eq!(outgoing.name, name.1);
                assert_eq!(outgoing.namespace, namespace.1);
                assert_eq!(outgoing.container.as_deref(), container.1);
            },
        );
    }
}
// END
