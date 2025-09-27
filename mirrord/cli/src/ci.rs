use std::path::PathBuf;

use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::{Progress, ProgressTracker};

use crate::{CiArgs, CiCommand, CliError, CliResult};

/// Handle commands related to CI `mirrord ci ...`
pub(crate) async fn ci_command(args: CiArgs) -> CliResult<()> {
    match args.command {
        CiCommand::ApiKey { config_file } => generate_ci_api_key(config_file).await,
    }
}

/// Generate a new API key for CI usage by calling the operator API:
/// `POST /mirrordclusteroperatorusercredentials`
#[tracing::instrument(level = "trace", ret)]
async fn generate_ci_api_key(config_file: Option<PathBuf>) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord ci api-key");

    let mut cfg_context =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, config_file);
    let layer_config = LayerConfig::resolve(&mut cfg_context).inspect_err(|error| {
        progress.failure(Some(&format!("failed to read config from env: {error}")));
    })?;

    let operator_api = OperatorApi::try_new(&layer_config, &mut NullReporter::default(), &progress)
        .await?
        .ok_or_else(|| {
            progress.failure(Some("operator not found"));
            CliError::OperatorNotInstalled
        })?;

    operator_api.check_license_validity(&progress)?;

    let mut subtask = progress.subtask("creating API key");
    let api_key = operator_api
        .create_ci_api_key()
        .await
        .inspect_err(|error| {
            subtask.failure(Some(&format!("failed to create API key: {error}")));
        })?;
    subtask.success(Some(&format!(
        r#"mirrord CI API key:
{api_key}

Please store this securely! To use it in your CI/CD system, set it as the value of the
MIRRORD_CI_API_KEY environment variable.
"#
    )));

    progress.success(None);
    Ok(())
}
