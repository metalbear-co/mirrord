use std::str::FromStr;

use serde::Deserialize;
use thiserror::Error;

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum IncomingConfig {
    Mirror,
    Steal,
}

impl Default for IncomingConfig {
    fn default() -> Self {
        IncomingConfig::Mirror
    }
}

#[derive(Error, Debug)]
#[error("could not parse IncomingConfig from string, values must be bool or mirror/steal")]
pub struct IncomingConfigParseError;

impl FromStr for IncomingConfig {
    type Err = IncomingConfigParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val.parse::<bool>() {
            Ok(true) => Ok(IncomingConfig::Steal),
            Ok(false) => Ok(IncomingConfig::Mirror),
            Err(_) => match val {
                "steal" => Ok(IncomingConfig::Steal),
                "mirror" => Ok(IncomingConfig::Mirror),
                _ => Err(IncomingConfigParseError),
            },
        }
    }
}

impl IncomingConfig {
    pub fn is_steal(&self) -> bool {
        self == &IncomingConfig::Steal
    }
}
