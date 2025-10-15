use std::{
    collections::HashMap,
    env::{self, temp_dir},
    path::{Path, PathBuf},
    process::Stdio,
};

use drain::Watch;
use fs4::tokio::AsyncFileExt;
use mirrord_analytics::NullReporter;
use mirrord_auth::credentials::CiApiKey;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::{Progress, ProgressTracker};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt};
use tracing::Level;

use crate::{CiArgs, CiCommand, CliError, CliResult, ci::error::CiError, user_data::UserData};

pub(crate) mod error;
pub(super) mod start;
pub(crate) mod stop;

/// Env var that the user has to set in order to execute `mirrord ci start` and `mirrord ci stop`
/// commands.
///
/// Should be set in their CI to the value they got from [`generate_ci_api_key`].
const MIRRORD_CI_API_KEY: &str = "MIRRORD_CI_API_KEY";

/// Alias for mirrord-for-ci results.
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
            .map(|_| ())?),
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

/// Stores the [`MirrordCi`] information we need to execute commands like `mirrord ci start|stop`.
///
/// - Note that it does **not** store the [`CiApiKey`], this one lives only as an env var.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct MirrordCiStore {
    /// pid of the intproxy, stored when the intproxy starts.
    intproxy_pid: Option<u32>,

    /// pid of the user process, stored when we spawn the user binary with mirrord.
    user_pid: Option<u32>,
}

impl MirrordCiStore {
    /// File where we store the intproxy pid, user process pid, and whatever else we need for
    /// [`MirrordCi`].
    const MIRRORD_FOR_CI_TMP_FILE_PATH: &str = "mirrord/mirrord-for-ci.json";

    /// Saves this [`MirrordCiStore`] to the file at [`Self::MIRRORD_FOR_CI_TMP_FILE_PATH`],
    /// creating a new file if it needed.
    async fn write_to_file(&self) -> CiResult<()> {
        let mut store_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(temp_dir().join(Self::MIRRORD_FOR_CI_TMP_FILE_PATH))
            .await?;

        if store_file.try_lock_exclusive()? {
            let contents = serde_json::to_vec(self)?;
            store_file.write_all(contents.as_slice()).await?;
            store_file.unlock()?;
        }

        Ok(())
    }

    /// Tries to read the [`MirrordCiStore`] from the path [`Self::MIRRORD_FOR_CI_TMP_FILE_PATH`],
    /// if it doesn't exist, then we return a [`Default`].
    async fn read_from_file_or_default() -> CiResult<Self> {
        match fs::read(temp_dir().join(Self::MIRRORD_FOR_CI_TMP_FILE_PATH)).await {
            Ok(contents) => Ok(serde_json::from_slice(contents.as_slice())?),
            Err(fail) if matches!(fail.kind(), std::io::ErrorKind::NotFound) => {
                Ok(MirrordCiStore::default())
            }
            Err(fail) => Err(fail.into()),
        }
    }

    /// Removes the [`MirrordCiStore`] file at [`Self::MIRRORD_FOR_CI_TMP_FILE_PATH`].
    async fn remove_file() -> CiResult<()> {
        Ok(tokio::fs::remove_file(temp_dir().join(Self::MIRRORD_FOR_CI_TMP_FILE_PATH)).await?)
    }
}

/// mirrord-for-ci operations require a [`CiApiKey`] to run.
///
/// `mirrord ci start` stores the pids of the user process and our intproxy so that we can
/// stop these processes on `mirrord ci stop`.
#[derive(Debug)]
pub(super) struct MirrordCi {
    /// Used as the `Credentials` (certificate) for the `mirrord ci` operations.
    ci_api_key: Option<CiApiKey>,

    /// [`MirrordCiStore`] holds the intproxy pid, and the user process pid so we can kill them
    /// later (on `mirrord ci stop`).
    ///
    /// The `store` may be initialized with default values, and we fill it as we get the pids.
    store: MirrordCiStore,
}

impl MirrordCi {
    /// Helper to access the [`CiApiKey`] when preparing the kube request headers.
    pub(super) fn api_key(&self) -> Option<&CiApiKey> {
        self.ci_api_key.as_ref()
    }

    /// When intproxy starts, we need to retrieve its pid and store it in the [`Self::store`], so we
    /// can kill the intproxy later.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn prepare_intproxy() -> CiResult<()> {
        let mut mirrord_ci_store = MirrordCiStore::read_from_file_or_default().await?;

        if mirrord_ci_store.intproxy_pid.is_some() {
            Err(CiError::IntproxyPidAlreadyPresent)
        } else {
            mirrord_ci_store.intproxy_pid = Some(std::process::id());
            Ok(mirrord_ci_store.write_to_file().await?)
        }
    }

    /// Prepares and runs the user binary with mirrord.
    ///
    /// Very similar to to how `mirrord exec` behaves, except that here we `spawn` a child process
    /// that'll keep running, and we store the pid of this process in [`MirrordCiStore`] so we can
    /// kill it later.
    #[tracing::instrument(level = Level::TRACE, skip(progress), err)]
    pub(super) async fn prepare_command<P: Progress>(
        self,
        progress: &mut P,
        binary: &str,
        binary_path: &Path,
        binary_args: &[String],
        env_vars: &HashMap<String, String>,
    ) -> CiResult<()> {
        let mut mirrord_ci_store = MirrordCiStore::read_from_file_or_default().await?;

        if mirrord_ci_store.user_pid.is_some() {
            Err(CiError::UserPidAlreadyPresent)
        } else {
            match tokio::process::Command::new(binary_path)
                .args(binary_args.iter().skip(1))
                .envs(env_vars)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(false)
                .spawn()
            {
                Ok(child) => {
                    mirrord_ci_store.user_pid = child.id();
                    mirrord_ci_store.write_to_file().await?;

                    progress.success(Some(&format!("child pid: {:?}", mirrord_ci_store.user_pid)));
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

    /// Reads the [`MirrordCiStore`], and the env var [`MIRRORD_CI_API_KEY`] to return a valid
    /// [`MirrordCi`].
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub(super) async fn get() -> CiResult<Self> {
        let store = MirrordCiStore::read_from_file_or_default().await?;

        let ci_api_key = match std::env::var(MIRRORD_CI_API_KEY) {
            Ok(api_key) => Some(CiApiKey::decode(&api_key)?),
            Err(env::VarError::NotPresent) => None,
            Err(fail @ env::VarError::NotUnicode(..)) => {
                return Err(CiError::EnvVar(MIRRORD_CI_API_KEY, fail));
            }
        };

        Ok(Self { ci_api_key, store })
    }

    /// Removes the [`MirrordCiStore`] files.
    ///
    /// If one of these files remains available, `mirrord ci start` may fail to execute, as you
    /// always need to match a `mirrord ci start` with a `mirrord ci stop`.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn clear(self) -> CiResult<()> {
        MirrordCiStore::remove_file().await
    }
}
