use std::{
    collections::HashMap,
    env::temp_dir,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
};

use drain::Watch;
use mirrord_analytics::NullReporter;
use mirrord_auth::credentials::CiApiKey;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::{Progress, ProgressTracker};
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tracing::Level;

use crate::{CiArgs, CiCommand, CliError, CliResult, ci::error::CiError, user_data::UserData};

pub(crate) mod error;
pub(super) mod start;
pub(crate) mod stop;

const MIRRORD_CI_API_KEY: &str = "MIRRORD_CI_API_KEY";

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
    user_pid: Option<u32>,
}

impl MirrordCi {
    const MIRRORD_FOR_CI_INTPROXY_TMP_FILE: &str = "mirrord/mirrord-for-ci-intproxy-pid";
    const MIRRORD_FOR_CI_USER_TMP_FILE: &str = "mirrord/mirrord-for-ci-user-pid";

    pub(super) fn api_key(&self) -> &CiApiKey {
        &self.ci_api_key
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn prepare_intproxy() -> CiResult<()> {
        if MirrordCi::get().await?.intproxy_pid.is_some() {
            Err(CiError::IntproxyPidAlreadyPresent)
        } else {
            let tmp_pid_file = temp_dir().join(Self::MIRRORD_FOR_CI_INTPROXY_TMP_FILE);

            let mut tmp_file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(tmp_pid_file)
                .await?;

            tmp_file
                .write_all(&std::process::id().to_be_bytes())
                .await?;

            Ok(())
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(progress), err)]
    pub(super) async fn prepare_command<P: Progress>(
        self,
        progress: &mut P,
        binary: &str,
        binary_path: &Path,
        binary_args: &[String],
        env_vars: &HashMap<String, String>,
    ) -> CiResult<()> {
        if MirrordCi::get().await?.user_pid.is_some() {
            Err(CiError::UserPidAlreadyPresent)
        } else {
            match tokio::process::Command::new(binary_path)
                .args(binary_args.into_iter().skip(1))
                .envs(env_vars)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(false)
                .spawn()
            {
                Ok(child) => {
                    if let Some(user_pid) = child.id() {
                        let tmp_pid_file = temp_dir().join(Self::MIRRORD_FOR_CI_USER_TMP_FILE);
                        let mut tmp_file = OpenOptions::new()
                            .create(true)
                            .write(true)
                            .truncate(true)
                            .open(tmp_pid_file)
                            .await?;

                        tmp_file.write_all(&user_pid.to_be_bytes()).await?;

                        progress.success(Some(&format!("child pid: {user_pid}")));
                    }

                    Ok::<_, CiError>(())
                }
                Err(fail) => {
                    progress.failure(Some(&fail.to_string()));
                    Err(CiError::BinaryExecuteFailed(
                        binary.to_string(),
                        binary_args.to_vec(),
                    ))
                }
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub(super) async fn get() -> CiResult<Self> {
        let mirrord_ci_intproxy_file = temp_dir().join(Self::MIRRORD_FOR_CI_INTPROXY_TMP_FILE);
        let intproxy_pid = match OpenOptions::new()
            .read(true)
            .open(mirrord_ci_intproxy_file)
            .await
        {
            Ok(mut pid_file) => Ok(Some(pid_file.read_u32().await?)),
            Err(fail) if matches!(fail.kind(), ErrorKind::NotFound) => Ok(None),
            Err(fail) => Err(fail),
        }?;

        let mirrord_ci_user_file = temp_dir().join(Self::MIRRORD_FOR_CI_USER_TMP_FILE);
        let user_pid = match OpenOptions::new()
            .read(true)
            .open(mirrord_ci_user_file)
            .await
        {
            Ok(mut pid_file) => Ok(Some(pid_file.read_u32().await?)),
            Err(fail) if matches!(fail.kind(), ErrorKind::NotFound) => Ok(None),
            Err(fail) => Err(fail),
        }?;

        let ci_api_key = std::env::var(MIRRORD_CI_API_KEY)
            .map_err(|fail| CiError::EnvVar(MIRRORD_CI_API_KEY, fail))?;

        Ok(Self {
            ci_api_key: CiApiKey::decode(&ci_api_key)?,
            intproxy_pid,
            user_pid,
        })
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn clear(self) -> CiResult<()> {
        tokio::fs::remove_file(temp_dir().join(Self::MIRRORD_FOR_CI_INTPROXY_TMP_FILE)).await?;
        tokio::fs::remove_file(temp_dir().join(Self::MIRRORD_FOR_CI_USER_TMP_FILE)).await?;

        Ok(())
    }
}
