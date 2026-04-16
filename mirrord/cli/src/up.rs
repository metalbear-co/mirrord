//! The `mirrord up` command - runs multiple mirrord sessions from a `mirrord-up.yaml` file.

use miette::Diagnostic;
use mirrord_config::config::EnvKey;
use mirrord_up::{UpError, load_up_config};
use thiserror::Error;

use crate::config::UpArgs;

#[derive(Debug, Error, Diagnostic)]
pub enum UpCliError {
    #[error(transparent)]
    Up(#[from] UpError),

    #[error("failed to acquire OS username for key")]
    #[diagnostic(help(
        "The username is used for automatically generating a session key. Please provide a session key manually or ensure your OS user has a valid username."
    ))]
    UsernameFetch(whoami::Error),
}

/// The `mirrord up` command handler.
pub(crate) async fn up_command(args: UpArgs) -> Result<(), UpCliError> {
    let up_config = load_up_config(&args.config_file)?;

    let key = match args.key {
        Some(key) => EnvKey::Provided(key),
        None => EnvKey::Generated(whoami::username().map_err(UpCliError::UsernameFetch)?),
    };

    Ok(mirrord_up::run(up_config, key).await?)
}
