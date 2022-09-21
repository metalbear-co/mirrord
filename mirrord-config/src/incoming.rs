use std::str::FromStr;

use serde::Deserialize;
use thiserror::Error;

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum IncomingField {
    Mirror,
    Steal,
}

impl Default for IncomingField {
    fn default() -> Self {
        IncomingField::Mirror
    }
}

#[derive(Error, Debug)]
#[error("could not parse IncomingField from string, values must be bool or mirror/steal")]
pub struct IncomingFieldParseError;

impl FromStr for IncomingField {
    type Err = IncomingFieldParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val.parse::<bool>() {
            Ok(true) => Ok(IncomingField::Steal),
            Ok(false) => Ok(IncomingField::Mirror),
            Err(_) => match val {
                "steal" => Ok(IncomingField::Steal),
                "mirror" => Ok(IncomingField::Mirror),
                _ => Err(IncomingFieldParseError),
            },
        }
    }
}

impl IncomingField {
    pub fn is_steal(&self) -> bool {
        self == &IncomingField::Steal
    }
}
