use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::ConfigError,
    util::{MirrordFlaggedConfig, VecOrSingle},
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct EnvField {
    #[from_env("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")]
    pub include: Option<VecOrSingle<String>>,

    #[from_env("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,
}

impl MirrordFlaggedConfig for EnvField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(MappedEnvField {
            include: std::env::var("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")
                .ok()
                .and_then(|val| val.parse().ok()),
            exclude: std::env::var("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")
                .ok()
                .and_then(|val| val.parse().ok()),
        })
    }
}
