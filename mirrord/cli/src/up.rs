//! The `mirrord up` command - runs multiple mirrord sessions from a `mirrord-up.yaml` file.

use mirrord_config::config::EnvKey;
use mirrord_up::load_up_config;

use crate::{config::UpArgs, error::CliResult};

/// The `mirrord up` command handler.
pub(crate) async fn up_command(args: UpArgs) -> CliResult<()> {
    let up_config = load_up_config(&args.config_file).map_err(crate::error::CliError::Up)?;
    let key = args
        .key
        .map(EnvKey::Provided)
        .unwrap_or_else(|| EnvKey::Generated(whoami::username().unwrap()));

    Ok(mirrord_up::run(up_config, key).await?)
}
