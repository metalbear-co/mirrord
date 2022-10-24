use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use super::FsModeConfig;
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = FsConfig)]
pub struct AdvancedFsUserConfig {
    #[serde(default)]
    #[config(nested)]
    pub mode: FsModeConfig,

    // TODO(alex) [low] 2022-10-14: It would be nice if we could set `env = GLOBAL` to avoid
    // repetition, but I think the `config` macro is taking a `Lit`?
    #[config(env = "MIRRORD_FILE_PATH_INCLUDE")]
    pub include: Option<VecOrSingle<String>>,

    #[config(env = "MIRRORD_FILE_PATH_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,
}

impl Default for FsConfig {
    fn default() -> Self {
        Self {
            mode: Default::default(),
            include: Default::default(),
            exclude: Default::default(),
        }
    }
}

impl MirrordToggleableConfig for AdvancedFsUserConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config()?;
        let include = FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE").source_value();
        let exclude = FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE").source_value();

        Ok(Self::Generated {
            mode,
            include,
            exclude,
        })
    }
}

#[cfg(test)]
mod tests {}
