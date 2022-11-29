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
    pub enabeld: bool,

    /// Direct Connect to operator addr
    #[config(env = "MIRRORD_OPERATOR")]
    pub addr: Option<String>,
}

impl MirrordToggleableConfig for OperatorFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(OperatorConfig {
            enabeld: FromEnv::new("MIRRORD_OPERATOR")
                .source_value()
                .unwrap_or(Ok(false))?,
            addr: FromEnv::new("MIRRORD_OPERATOR")
                .source_value()
                .transpose()?,
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
        #[values((None, true), (Some("false"), false))] enabeld: (Option<&str>, bool),
        #[values((None, None), (Some("foobar"), Some("foobar")))] addr: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_OPERATOR_ENABLE", enabeld.0),
                ("MIRRORD_OPERATOR", addr.0),
            ],
            || {
                let operator = OperatorFileConfig::default().generate_config().unwrap();

                assert_eq!(operator.enabeld, enabeld.1);
                assert_eq!(operator.addr.as_deref(), addr.1);
            },
        );
    }
}
