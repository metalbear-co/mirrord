use std::num::ParseIntError;

use mirrord_config::config::ConfigError;
use thiserror::Error;

use crate::{error::ContainerError, CliError};

#[derive(Debug, Error)]
pub(crate) enum ComposeError {
    #[error(transparent)]
    CLI(#[from] CliError),

    #[error(transparent)]
    Container(#[from] ContainerError),

    #[error(transparent)]
    IO(#[from] std::io::Error),

    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    #[error("Compose file is missing the field `{0}`!")]
    MissingField(String),

    #[error("Compose value expected type `{0}`!")]
    UnexpectedType(String),

    #[error(transparent)]
    LayerConfig(#[from] ConfigError),

    #[error(transparent)]
    ParseInt(#[from] ParseIntError),
}
