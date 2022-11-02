use std::collections::HashMap;

use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = EnvConfig)]
pub struct EnvFileConfig {
    #[config(env = "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")]
    pub include: Option<VecOrSingle<String>>,

    #[config(env = "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,

    /// Set or override environment variables.
    #[serde(rename = "override")]
    pub overrides: Option<HashMap<String, String>>,
}

impl MirrordToggleableConfig for EnvFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(EnvConfig {
            include: FromEnv::new("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE").source_value(),
            exclude: FromEnv::new("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE").source_value(),
            overrides: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, ToggleableConfig},
    };

    #[rstest]
    fn default(
        #[values((None, None), (Some("IVAR1"), Some("IVAR1")), (Some("IVAR1;IVAR2"), Some("IVAR1;IVAR2")))]
        include: (Option<&str>, Option<&str>),
        #[values((None, None), (Some("EVAR1"), Some("EVAR1")))] exclude: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE", include.0),
                ("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE", exclude.0),
            ],
            || {
                let env = EnvFileConfig::default().generate_config().unwrap();

                assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
                assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
            },
        );
    }

    #[rstest]
    fn disabled_config(
        #[values((None, None), (Some("IVAR1"), Some("IVAR1")))] include: (
            Option<&str>,
            Option<&str>,
        ),
        #[values((None, None), (Some("EVAR1"), Some("EVAR1")))] exclude: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE", include.0),
                ("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE", exclude.0),
            ],
            || {
                let env = ToggleableConfig::<EnvFileConfig>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
                assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
            },
        );
    }
}
