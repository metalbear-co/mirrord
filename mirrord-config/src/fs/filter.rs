use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

const INCLUDE: &str = "MIRRORD_FILE_PATH_INCLUDE";
const EXCLUDE: &str = "MIRRORD_FILE_PATH_EXCLUDE";

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = FileFilterConfig)]
pub struct FileFilterUserConfig {
    // TODO(alex) [low] 2022-10-14: It would be nice if we could set `env = GLOBAL` to avoid
    // repetition, but I think the `config` macro is taking a `Lit`?
    #[config(env = "MIRRORD_FILE_PATH_INCLUDE")]
    pub include: Option<VecOrSingle<String>>,

    #[config(env = "MIRRORD_FILE_PATH_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,
}

pub struct FileFilterConfig {
    pub include: Option<VecOrSingle<String>>,
    pub exclude: Option<VecOrSingle<String>>,
}

impl Default for FileFilterConfig {
    fn default() -> Self {
        Self {
            include: Default::default(),
            exclude: Default::default(),
        }
    }
}

impl MirrordToggleableConfig for FileFilterUserConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let include = FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE").source_value();
        let exclude = FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE").source_value();

        Ok(Self::Generated { include, exclude })
    }
}

#[cfg(test)]
mod tests {}
