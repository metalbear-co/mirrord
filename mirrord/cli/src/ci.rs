use std::{env::temp_dir, path::PathBuf, process::ExitStatus};

use drain::Watch;
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::{Progress, ProgressTracker};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tracing::Level;

use crate::{
    CiArgs, CiCommand, CliError, CliResult, ExecArgs, error::InternalProxyError, exec,
    user_data::UserData,
};

const MIRRORD_CI_API_KEY: &str = "MIRRORD_CI_API_KEY";

/// Handle commands related to CI `mirrord ci ...`
pub(crate) async fn ci_command(
    args: CiArgs,
    watch: Watch,
    user_data: &mut UserData,
) -> CliResult<()> {
    match args.command {
        CiCommand::ApiKey { config_file } => generate_ci_api_key(config_file).await,
        CiCommand::Start(exec_args) => {
            CiStartCommandHandler::new(exec_args, watch, user_data)
                .await?
                .handle()
                .await
        }
        CiCommand::Stop => CiStopCommandHandler::handle()
            .await
            .map(|status| tracing::info!(?status, "Kill all!")),
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

pub(super) struct CiStartCommandHandler<'a> {
    mirrord_for_ci: MirrordCi,
    exec_args: Box<ExecArgs>,
    watch: Watch,
    user_data: &'a mut UserData,
}

impl<'a> CiStartCommandHandler<'a> {
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(
        exec_args: Box<ExecArgs>,
        watch: Watch,
        user_data: &'a mut UserData,
    ) -> CliResult<Self> {
        let ci_api_key = std::env::var(MIRRORD_CI_API_KEY)
            .map_err(|fail| CliError::EnvVar(MIRRORD_CI_API_KEY, fail))?;

        // TODO(alex) [mid]: User cannot set `operator: false`, we need the operator for this one.
        let mirrord_for_ci = MirrordCi { ci_api_key };

        unsafe {
            std::env::set_var("MIRRORD_PROGRESS_MODE", "off");
        }

        Ok(Self {
            mirrord_for_ci,
            exec_args,
            watch,
            user_data,
        })
    }

    pub(super) async fn handle(self) -> CliResult<()> {
        let Self {
            mirrord_for_ci,
            exec_args,
            watch,
            user_data,
        } = self;

        exec(&exec_args, watch, user_data, Some(mirrord_for_ci)).await
    }
}

pub(super) struct CiStopCommandHandler;

impl CiStopCommandHandler {
    pub(super) async fn handle() -> CliResult<ExitStatus> {
        let ci_info = MirrordCiInfo::read().await?;

        Ok(Command::new("kill")
            .arg(ci_info.intproxy_pid.to_string())
            .spawn()?
            .wait()
            .await?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MirrordCiInfo {
    intproxy_pid: u32,
}

impl MirrordCiInfo {
    pub(super) async fn prepare_intproxy() -> Result<(), InternalProxyError> {
        if std::env::var(MIRRORD_CI_API_KEY).is_ok() {
            let mut tmp_mirrord = temp_dir();
            tmp_mirrord.push("mirrord/mirrord-for-ci-info.json");

            let mut mirrord_ci_file = tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(tmp_mirrord)
                .await?;

            let mut contents = String::new();
            mirrord_ci_file.read_to_string(&mut contents).await?;

            let new_contents: Self = if contents.is_empty() {
                Self {
                    intproxy_pid: std::process::id(),
                }
            } else {
                let mut old_contents: Self = serde_json::from_str(&contents)?;
                old_contents.intproxy_pid = std::process::id();

                old_contents
            };

            mirrord_ci_file
                .write(serde_json::to_string_pretty(&new_contents)?.as_bytes())
                .await?;
        }

        Ok(())
    }

    pub(super) async fn read() -> Result<Self, InternalProxyError> {
        let mut tmp_mirrord = temp_dir();
        tmp_mirrord.push("mirrord/mirrord-for-ci-info.json");

        let mut mirrord_ci_file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(tmp_mirrord)
            .await?;

        let mut contents = String::new();
        mirrord_ci_file.read_to_string(&mut contents).await?;

        Ok(serde_json::from_str(&contents)?)
    }
}

#[derive(Debug)]
pub(super) struct MirrordCi {
    ci_api_key: String,
}

impl MirrordCi {
    pub(super) fn api_key(&self) -> &str {
        &self.ci_api_key
    }
}
