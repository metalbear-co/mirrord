use std::{
    path::PathBuf,
    process::{ExitStatus, Stdio},
};

use miette::Diagnostic;
use mirrord_config::config::{ConfigError, EnvKey};
use thiserror::Error;
mod config;

pub use config::{SubprocessCfg, UpConfig};
use mirrord_progress::MIRRORD_PROGRESS_ENV;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    task::{JoinError, JoinSet},
};

/// Errors produced by `mirrord up` command.
#[derive(Debug, Error, Diagnostic)]
pub enum UpError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to parse mirrord-up config: {0}")]
    #[diagnostic(help("Check the YAML syntax and field names in your mirrord-up.yaml."))]
    Parse(#[from] serde_yaml::Error),
}

/// Load and parse a `mirrord-up.yaml` configuration file.
pub fn load_up_config(path: &PathBuf) -> Result<UpConfig, UpError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&content)?)
}
