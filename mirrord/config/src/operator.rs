use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::MirrordToggleableConfig,
};
/// Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "OperatorFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct OperatorConfig {
    #[config(env = "MIRRORD_OPERATOR_ENABLE", default = true)]
    pub enabled: bool,

    #[config(env = "MIRRORD_OPERATOR_NAMESPACE", default = "mirrord")]
    pub namespace: String,

    #[config(env = "MIRRORD_OPERATOR_PORT", default = 8080)]
    pub port: u16,
}

impl MirrordToggleableConfig for OperatorFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(OperatorConfig {
            enabled: FromEnv::new("MIRRORD_OPERATOR_ENABLE")
                .source_value()
                .unwrap_or(Ok(false))?,
            namespace: FromEnv::new("MIRRORD_OPERATOR_NAMESPACE")
                .source_value()
                .transpose()?
                .unwrap_or_else(|| "mirrord".to_owned()),
            port: FromEnv::new("MIRRORD_OPERATOR_PORT")
                .source_value()
                .transpose()?
                .unwrap_or(8080),
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((None, true), (Some("false"), false))] enabled: (Option<&str>, bool),
        #[values((None, "mirrord"), (Some("foobar"), "foobar"))] namespace: (Option<&str>, &str),
        #[values((None, 8080), (Some("3000"), 3000))] port: (Option<&str>, u16),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_OPERATOR_ENABLE", enabled.0),
                ("MIRRORD_OPERATOR_NAMESPACE", namespace.0),
                ("MIRRORD_OPERATOR_PORT", port.0),
            ],
            || {
                let operator = OperatorFileConfig::default().generate_config().unwrap();

                assert_eq!(operator.enabled, enabled.1);
                assert_eq!(operator.namespace, namespace.1);
                assert_eq!(operator.port, port.1);
            },
        );
    }
}
