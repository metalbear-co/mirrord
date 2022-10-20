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
pub enum FsFileConfig {
    Simple(FsModeConfig),
    Advanced(FsConfig),
}

impl Default for FsFileConfig {
    fn default() -> Self {
        FsFileConfig::Simple(FsModeConfig::Read)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FsConfig {
    pub mode: FsModeConfig,
    pub filter: FileFilterConfig,
}

impl MirrordConfig for FsFileConfig {
    type Generated = FsConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let config = match self {
            FsFileConfig::Simple(mode) => FsConfig {
                mode,
                filter: Default::default(),
            },
            FsFileConfig::Advanced(fs_config) => fs_config,
        };

        Ok(config)
    }
}

impl MirrordToggleableConfig for FsFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config()?;
        let filter = FileFilterConfig::disabled_config()?;

        Ok(FsConfig { mode, filter })
    }
}
