use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordFlaggedConfig,
};

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
        let read = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();
        let write = FromEnv::new("MIRRORD_FILE_OPS").source_value();

        Ok(match (read, write) {
            (Some(true), Some(false)) => FsField::Read,
            (_, Some(true)) => FsField::Write,
            _ => self,
        })
    }
}

impl MirrordFlaggedConfig for FsField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let read = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();
        let write = FromEnv::new("MIRRORD_FILE_OPS").source_value();

        Ok(match (read, write) {
            (Some(true), Some(false)) => FsField::Read,
            (_, Some(true)) => FsField::Write,
            _ => FsField::Disabled,
        })
    }
}
