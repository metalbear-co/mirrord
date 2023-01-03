use std::str::FromStr;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::MirrordToggleableConfig,
};

#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "IncomingFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct IncomingConfig {
    #[config(env = "MIRRORD_TCP_OUTGOING", default = IncomingMode::Mirror)]
    pub mode: IncomingMode,

    #[config(env = "MIRRORD_HTTP_FILTER")]
    pub filter: Option<String>,
}

impl MirrordToggleableConfig for IncomingFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let filter = FromEnv::new("MIRRORD_HTTP_FILTER")
            .source_value()
            .transpose()?;

        let mode = FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
            .source_value()
            .unwrap_or_else(|| Ok(Default::default()))?;

        Ok(IncomingConfig { mode, filter })
    }
}

impl IncomingConfig {
    pub fn is_steal(&self) -> bool {
        matches!(self.mode, IncomingMode::Steal)
    }
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum IncomingMode {
    #[default]
    Mirror,
    Steal,
}

#[derive(Error, Debug)]
#[error("could not parse IncomingConfig from string, values must be bool or mirror/steal")]
pub struct IncomingConfigParseError;

impl FromStr for IncomingMode {
    type Err = IncomingConfigParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val.parse::<bool>() {
            Ok(true) => Ok(Self::Steal),
            Ok(false) => Ok(Self::Mirror),
            Err(_) => match val {
                "steal" => Ok(Self::Steal),
                "mirror" => Ok(Self::Mirror),
                _ => Err(IncomingConfigParseError),
            },
        }
    }
}
