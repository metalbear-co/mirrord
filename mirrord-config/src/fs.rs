use serde::Deserialize;

pub use self::{advanced::*, mode::*};
use crate::{
    config::{ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

pub mod advanced;
pub mod mode;

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FsUserConfig {
    Simple(FsModeConfig),
    Advanced(AdvancedFsUserConfig),
}

impl Default for FsUserConfig {
    fn default() -> Self {
        FsUserConfig::Simple(FsModeConfig::Read)
    }
}

impl MirrordConfig for FsUserConfig {
    type Generated = FsConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let config = match self {
            FsUserConfig::Simple(mode) => FsConfig {
                mode,
                include: Default::default(),
                exclude: Default::default(),
            },
            FsUserConfig::Advanced(AdvancedFsUserConfig {
                mode,
                include,
                exclude,
            }) => FsConfig {
                mode,
                include,
                exclude,
            },
        };

        Ok(config)
    }
}

impl MirrordToggleableConfig for FsUserConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config()?;

        Ok(FsConfig {
            mode,
            include: todo!(),
            exclude: todo!(),
        })
    }
}

impl FsConfig {
    pub fn is_read(&self) -> bool {
        self.mode.is_read()
    }

    pub fn is_write(&self) -> bool {
        self.mode.is_write()
    }
}
