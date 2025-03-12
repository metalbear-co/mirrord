use std::io::{self, Write};

use mirrord_config::{config::ConfigError, LayerConfig};
use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ResolvedConfigError {
    #[error(transparent)]
    EncodeError(#[from] ConfigError),
    #[error(transparent)]
    IoError(#[from] io::Error),
    #[error("temporary file path is not valid UTF-8: {0}")]
    NonUtf8Path(String),
}

#[derive(Debug)]
pub struct ResolvedConfigFile {
    file: NamedTempFile,
}

impl ResolvedConfigFile {
    pub fn try_new(config: &LayerConfig) -> Result<Self, ResolvedConfigError> {
        let encoded = config.encode()?;
        let mut file = NamedTempFile::new()?;
        file.write_all(encoded.as_bytes())?;
        Ok(Self { file })
    }

    pub fn path_str(&self) -> Result<&str, ResolvedConfigError> {
        self.file.path().to_str().ok_or_else(|| {
            ResolvedConfigError::NonUtf8Path(self.file.path().to_string_lossy().into_owned())
        })
    }
}
