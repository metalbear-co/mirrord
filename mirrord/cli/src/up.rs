//! The `mirrord up` command - runs multiple mirrord sessions from a `mirrord-up.yaml` file.

use std::io::ErrorKind;

use miette::Diagnostic;
use mirrord_config::config::EnvKey;
use mirrord_up::{UpError, load_up_config};
use thiserror::Error;

use crate::config::UpArgs;

#[derive(Debug, Error, Diagnostic)]
pub enum UpCliError {
    #[error(transparent)]
    Up(#[from] UpError),

    #[error("mirrord-up.yaml file not found.")]
    #[diagnostic(help(
        "Please create a mirrord-up.yaml file or specify its exact path with `mirrord up -f <file path.yaml>`"
    ))]
    ConfigNotFound,

    #[error("failed to acquire OS username for key")]
    #[diagnostic(help(
        "The username is used for automatically generating a session key. Please provide a session key manually or ensure your OS user has a valid username."
    ))]
    UsernameFetch(whoami::Error),
}

/// The `mirrord up` command handler.
pub(crate) async fn up_command(args: UpArgs) -> Result<(), UpCliError> {
    let up_config = match load_up_config(&args.config_file) {
        Ok(cfg) => cfg,
        Err(UpError::Io(err)) if err.kind() == ErrorKind::NotFound => {
            return Err(UpCliError::ConfigNotFound);
        }
        other_error => other_error?,
    };

    let key = match args.key {
        Some(key) => EnvKey::Provided(key),
        None => EnvKey::Generated(whoami::username().map_err(UpCliError::UsernameFetch)?),
    };

    Ok(mirrord_up::run(up_config, key).await?)
}
