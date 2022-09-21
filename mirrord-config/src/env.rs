use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordFlaggedConfig, VecOrSingle},
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct EnvField {
    #[config(env = "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")]
    pub include: Option<VecOrSingle<String>>,

    #[config(env = "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,
}

impl MirrordFlaggedConfig for EnvField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(MappedEnvField {
            include: FromEnv::new("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE").source_value(),
            exclude: FromEnv::new("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE").source_value(),
        })
    }
}
