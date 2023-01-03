use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    incoming::IncomingConfig,
    outgoing::{OutgoingConfig, OutgoingFileConfig},
    util::MirrordToggleableConfig,
};




#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IncomingFileConfig {
    Simple(IncomingMode),
    Advanced(IncomingConfig),
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum IncomingConfig {
    Mirror,
    Steal { filter: Option<String> },
}

impl Default for IncomingFileConfig {
    fn default() -> Self {
        IncomingFileConfig::Simple(IncomingMode::Mirror)
    }
}

impl FromMirrordConfig for IncomingConfig {
    type Generator = IncomingFileConfig;
}

impl MirrordConfig for IncomingFileConfig{
    type Generated = IncomingConfig;

    fn generate_config(self) -> Result<Self::Generated> {
        let config = match self {
            TargetFileConfig::Simple(r#mode) => {
              /// Change from IncomingMode + env vars to IncomingConfig
            },
            TargetFileConfig::Advanced(config) => {
              /// Enrich IncomingConfig with env vars to IncomingConfig
            },
        };

        Ok(config)
    }
}