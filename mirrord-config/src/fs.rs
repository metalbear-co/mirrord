use serde::Deserialize;

pub use self::{filter::*, mode::*};
use crate::{
    config::{ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

pub mod filter;
pub mod mode;

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FsUserConfig {
    Simple(FsModeConfig),
    Advanced(FsConfig),
}

impl Default for FsUserConfig {
    fn default() -> Self {
        FsUserConfig::Simple(FsModeConfig::Read)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FsConfig {
    pub mode: FsModeConfig,
    pub filter: FileFilterConfig,
}

impl MirrordConfig for FsUserConfig {
    type Generated = FsConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let config = match self {
            FsUserConfig::Simple(mode) => FsConfig {
                mode,
                filter: Default::default(),
            },
            FsUserConfig::Advanced(fs_config) => fs_config,
        };

        Ok(config)
    }
}

impl MirrordToggleableConfig for FsUserConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config()?;
        let filter = FileFilterUserConfig::disabled_config()?;

        Ok(FsConfig { mode, filter })
    }
}
