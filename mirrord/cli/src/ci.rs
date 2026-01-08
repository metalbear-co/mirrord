use std::{
    collections::{HashMap, HashSet},
    env::{self, temp_dir},
    path::{Path, PathBuf},
};
#[cfg(unix)]
use std::{fs::File, os::unix::process::ExitStatusExt, process::Stdio, time::SystemTime};

use ci_info::types::CiInfo;
use drain::Watch;
use fs4::tokio::AsyncFileExt;
use mirrord_analytics::NullReporter;
use mirrord_auth::credentials::CiApiKey;
use mirrord_config::{LayerConfig, ci::CiConfig, config::ConfigContext};
use mirrord_operator::{client::OperatorApi, crd::session::SessionCiInfo};
use mirrord_progress::{Progress, ProgressTracker};
#[cfg(unix)]
use rand::distr::{Alphanumeric, SampleString};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use tokio::fs::create_dir_all;
use tokio::{fs, io::AsyncWriteExt};
use tracing::Level;

use crate::{
    CiArgs, CiCommand, CiStartArgs, CliError, CliResult, ci::error::CiError, user_data::UserData,
};

pub(crate) mod error;
pub(super) mod start;
pub(crate) mod stop;

/// Env var that the user has to set in order to execute `mirrord ci start` and `mirrord ci stop`
/// commands when the operator is available.
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
    intproxy_pids: HashSet<u32>,

    /// pid of the user process, stored when we spawn the user binary with mirrord.
    user_pids: HashSet<Option<u32>>,
}

impl MirrordCiStore {
    /// File where we store the intproxy pid, user process pid, and whatever else we need for
    /// [`MirrordCi`].
    const MIRRORD_FOR_CI_TMP_FILE_PATH: &str = "mirrord/mirrord-for-ci.json";

    /// Saves this [`MirrordCiStore`] to the file at [`Self::MIRRORD_FOR_CI_TMP_FILE_PATH`],
    /// creating a new file if it needed.
    async fn write_to_file(&self) -> CiResult<()> {
        let file_path = temp_dir().join(Self::MIRRORD_FOR_CI_TMP_FILE_PATH);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut store_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(file_path)
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
    #[cfg_attr(windows, allow(unused))]
    async fn remove_file() -> CiResult<()> {
        match tokio::fs::remove_file(temp_dir().join(Self::MIRRORD_FOR_CI_TMP_FILE_PATH)).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Check if the store is empty. Return `true` if no process is found.
    ///
    /// Used to avoid errors when calling `mirrord ci stop` multiple times, since that command
    /// should've `kill`ed everything already on the 1st run.
    #[cfg_attr(windows, allow(unused))]
    fn is_empty(&self) -> bool {
        self.intproxy_pids.is_empty() && self.user_pids.is_empty()
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

    /// Arguments that are specific to `mirrord ci start`.
    #[cfg_attr(windows, allow(unused))]
    start_args: StartArgs,
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
        mirrord_ci_store.intproxy_pids.insert(std::process::id());

        Ok(mirrord_ci_store.write_to_file().await?)
    }

    /// Prepares and runs the user binary with mirrord.
    ///
    /// Very similar to to how `mirrord exec` behaves, except that here we `spawn` a child process
    /// that'll keep running, and we store the pid of this process in [`MirrordCiStore`] so we can
    /// kill it later.
    #[cfg(not(target_os = "windows"))]
    #[tracing::instrument(level = Level::TRACE, skip(progress), err)]
    pub(super) async fn prepare_command<P: Progress>(
        self,
        progress: &mut P,
        binary: &str,
        binary_path: &Path,
        binary_args: &[String],
        env_vars: &HashMap<String, String>,
        CiConfig { output_dir }: &CiConfig,
    ) -> CiResult<()> {
        use nix::libc::{SIGINT, SIGKILL, SIGTERM};

        let mut mirrord_ci_store = MirrordCiStore::read_from_file_or_default().await?;

        // Create a dir like `/tmp/mirrord/node-1234-cool` where we dump ci related files.
        let ci_run_output_dir = {
            let parent_dir = output_dir
                .clone()
                .unwrap_or_else(|| temp_dir().join("mirrord"));

            let random_name: String = Alphanumeric.sample_string(&mut rand::rng(), 7);
            let timestamp = SystemTime::UNIX_EPOCH
                .elapsed()
                .expect("system time should not be earlier than UNIX EPOCH")
                .as_secs();

            let ci_run_output_dir = parent_dir.join(format!("{binary}-{timestamp}-{random_name}"));

            create_dir_all(&ci_run_output_dir).await?;

            ci_run_output_dir
        };

        progress.info(&format!(
            "mirrord ci files will be stored in {}",
            ci_run_output_dir.display()
        ));

        let mut child = match tokio::process::Command::new(binary_path)
            .args(binary_args.iter().skip(1))
            .envs(env_vars)
            .stdin(Stdio::null())
            .stdout(File::create(ci_run_output_dir.join("stdout"))?)
            .stderr(File::create(ci_run_output_dir.join("stderr"))?)
            .kill_on_drop(false)
            .spawn()
        {
            Ok(child) => {
                mirrord_ci_store.user_pids.insert(child.id());
                mirrord_ci_store.write_to_file().await?;
                child
            }
            Err(fail) => {
                progress.failure(Some(&fail.to_string()));
                return Err(CiError::BinaryExecuteFailed(
                    binary.to_string(),
                    binary_args.to_vec(),
                ));
            }
        };

        let child_pid = child
            .id()
            .map(|pid| pid.to_string())
            .unwrap_or("unknown".to_string());
        if self.start_args.foreground {
            progress.info(&format!("waiting for child with pid {child_pid}"));
            match child.wait().await {
                Ok(status) => {
                    if status.success() {
                        progress.success(None);
                    } else if let Some(signal) = status.signal() {
                        match signal {
                            SIGKILL => progress.success(Some("process killed by SIGKILL")),
                            SIGTERM => progress.success(Some("process terminated by SIGTERM")),
                            SIGINT => progress.success(Some("process interrupted by SIGINT")),
                            _ => progress
                                .failure(Some(&format!("process exited with status: {}", status))),
                        };
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
        } else {
            progress.success(Some(&format!("child pid: {child_pid}")));
            Ok::<_, CiError>(())
        }
    }

    #[cfg_attr(windows, allow(unused))]
    #[cfg(target_os = "windows")]
    pub(super) async fn prepare_command<P: Progress>(
        self,
        progress: &mut P,
        binary: &str,
        binary_path: &Path,
        binary_args: &[String],
        env_vars: &HashMap<String, String>,
        CiConfig { output_dir }: &CiConfig,
    ) -> CiResult<()> {
        unimplemented!("Not supported on windows.");
    }

    /// Reads the [`MirrordCiStore`], and the env var [`MIRRORD_CI_API_KEY`] to return a valid
    /// [`MirrordCi`].
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub(super) async fn new(args: &CiStartArgs) -> CiResult<Self> {
        MirrordCiStore::read_from_file_or_default().await?;
        let start_args = args.into();

        let ci_api_key = match std::env::var(MIRRORD_CI_API_KEY) {
            Ok(api_key) => Some(CiApiKey::decode(&api_key)?),
            Err(env::VarError::NotPresent) => None,
            Err(fail @ env::VarError::NotUnicode(..)) => {
                return Err(CiError::EnvVar(MIRRORD_CI_API_KEY, fail));
            }
        };

        Ok(Self {
            ci_api_key,
            start_args,
        })
    }

    /// Converts a [`CiInfo`] into a [`SessionCiInfo`] used by the operator.
    pub(super) fn info(&self) -> SessionCiInfo {
        let CiInfo { name, .. } = ci_info::get();

        let StartArgs {
            foreground: _,
            environment,
            pipeline,
            triggered_by,
        } = self.start_args.clone();

        SessionCiInfo {
            provider: name,
            environment,
            pipeline,
            triggered_by,
        }
    }
}

/// Similar to [`CiStartArgs`], except here we don't need [`CiStartArgs::exec_args`].
///
/// Used instead of `CiStartArgs` so we can `Clone` it around, (we don't need the `ExecArgs` where
/// this is used).
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Default, Clone)]
struct StartArgs {
    /// Runs mirrord ci in the foreground (the default behaviour is to run it as a background
    /// task).
    #[cfg_attr(windows, allow(unused))]
    foreground: bool,

    /// CI environment, e.g. "staging", "production", "testing", etc.
    environment: Option<String>,

    /// CI pipeline or job name, e.g. "e2e-tests".
    pipeline: Option<String>,

    /// CI pipeline trigger, e.g. "push", "pull request", "manual", etc.
    triggered_by: Option<String>,
}

impl From<&CiStartArgs> for StartArgs {
    fn from(
        CiStartArgs {
            exec_args: _,
            foreground,
            environment,
            pipeline,
            triggered_by,
        }: &CiStartArgs,
    ) -> Self {
        Self {
            foreground: *foreground,
            environment: environment.clone(),
            pipeline: pipeline.clone(),
            triggered_by: triggered_by.clone(),
        }
    }
}
