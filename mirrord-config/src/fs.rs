use serde::Deserialize;

use crate::util::{ConfigError, MirrordConfig, MirrordFlaggedConfig};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum FsField {
    Disabled,
    Read,
    Write,
}

impl FsField {
    pub fn is_read(&self) -> bool {
        self == &FsField::Read
    }

    pub fn is_write(&self) -> bool {
        self == &FsField::Write
    }
}

impl Default for FsField {
    fn default() -> Self {
        FsField::Read
    }
}

impl MirrordConfig for FsField {
    type Generated = FsField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let read = std::env::var("MIRRORD_FILE_RO_OPS")
            .ok()
            .and_then(|val| val.parse::<bool>().ok());

        let write = std::env::var("MIRRORD_FILE_OPS")
            .ok()
            .and_then(|val| val.parse::<bool>().ok());

        Ok(match (read, write) {
            (Some(true), Some(false)) => FsField::Read,
            (_, Some(true)) => FsField::Write,
            _ => self,
        })
    }
}

impl MirrordFlaggedConfig for FsField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let read = std::env::var("MIRRORD_FILE_RO_OPS")
            .ok()
            .and_then(|val| val.parse::<bool>().ok());

        let write = std::env::var("MIRRORD_FILE_OPS")
            .ok()
            .and_then(|val| val.parse::<bool>().ok());

        Ok(match (read, write) {
            (Some(true), Some(false)) => FsField::Read,
            (_, Some(true)) => FsField::Write,
            _ => FsField::Disabled,
        })
    }
}
