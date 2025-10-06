use std::{env, path::PathBuf};

use drain::Watch;
use mirrord_analytics::NullReporter;
use mirrord_auth::credentials::CiApiKey;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::{Progress, ProgressTracker};
use tracing::Level;

use crate::{CiArgs, CiCommand, CliError, CliResult, ci::error::CiError, user_data::UserData};

pub(crate) mod error;
pub(super) mod start;
pub(crate) mod stop;

const MIRRORD_CI_API_KEY: &str = "MIRRORD_CI_API_KEY";
const MIRRORD_FOR_CI_INTPROXY_PID: &str = "MIRRORD_FOR_CI_INTPROXY_PID";

type CiResult<T> = Result<T, crate::ci::error::CiError>;

/// Handle commands related to CI `mirrord ci ...`
pub(crate) async fn ci_command(
    args: CiArgs,
    watch: Watch,
    user_data: &mut UserData,
) -> CliResult<()> {
    match args.command {
        CiCommand::ApiKey { config_file } => generate_ci_api_key(config_file).await,
        CiCommand::Start(exec_args) => Ok(start::CiStartCommandHandler::new(
            exec_args, watch, user_data,
        )
        .await?
        .handle()
        .await?),
        CiCommand::Stop => Ok(stop::CiStopCommandHandler::new()
            .await?
            .handle()
            .await
            .map(|status| tracing::info!(?status, "Kill all!"))?),
    }
}

/// Generate a new API key for CI usage by calling the operator API:
/// `POST /mirrordclusteroperatorusercredentials`
#[tracing::instrument(level = Level::TRACE, ret)]
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

#[derive(Debug)]
pub(super) struct MirrordCi {
    ci_api_key: CiApiKey,
    intproxy_pid: Option<u32>,
}

impl MirrordCi {
    pub(super) fn api_key(&self) -> &CiApiKey {
        &self.ci_api_key
    }

    pub(super) fn prepare_intproxy() -> CiResult<()> {
        let mirrord_for_ci = MirrordCi::get()?;

        match mirrord_for_ci.intproxy_pid {
            Some(_) => Err(CiError::IntproxyPidAlreadyPresent),
            None => unsafe {
                env::set_var(MIRRORD_FOR_CI_INTPROXY_PID, std::process::id().to_string());
                Ok(())
            },
        }
    }

    pub(super) fn get() -> CiResult<Self> {
        let intproxy_pid: Option<u32> = match std::env::var(MIRRORD_FOR_CI_INTPROXY_PID) {
            Ok(pid) => Some(pid.parse()?),
            Err(std::env::VarError::NotPresent) => None,
            Err(fail @ env::VarError::NotUnicode(..)) => {
                Err(CiError::EnvVar(MIRRORD_FOR_CI_INTPROXY_PID, fail))?
            }
        };

        let ci_api_key = std::env::var(MIRRORD_CI_API_KEY)
            .map_err(|fail| CiError::EnvVar(MIRRORD_CI_API_KEY, fail))?;

        Ok(Self {
            ci_api_key: CiApiKey::decode(&ci_api_key)?,
            intproxy_pid,
        })
    }
}
