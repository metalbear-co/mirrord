use serde::Deserialize;

pub use self::{advanced::*, mode::*};
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

pub mod advanced;
pub mod mode;

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged, rename_all = "lowercase")]
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
        let include = FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE").source_value();
        let exclude = FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE").source_value();

        Ok(FsConfig {
            mode,
            include,
            exclude,
        })
    }
}


