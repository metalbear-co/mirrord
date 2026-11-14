#![cfg_attr(windows, allow(unused))]
use std::{
    collections::{HashMap, HashSet},
    env::{self, temp_dir},
    io,
    ops::Not,
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(not(target_os = "windows"))]
use std::{
    fs::File,
    os::unix::{fs::MetadataExt, process::ExitStatusExt},
    process::Stdio,
    time::SystemTime,
};

use ci_info::types::CiInfo;
use drain::Watch;
use fs4::tokio::AsyncFileExt;
use mirrord_analytics::NullReporter;
use mirrord_auth::credentials::CiApiKey;
#[cfg(not(target_os = "windows"))]
use mirrord_command::resolve_tokio_command;
use mirrord_config::{
    LayerConfig, ci::CiConfig, config::ConfigContext, container::ContainerRuntime,
};
use mirrord_operator::{client::OperatorApi, crd::session::SessionCiInfo};
use mirrord_progress::{Progress, ProgressTracker};
#[cfg(not(target_os = "windows"))]
use rand::distr::{Alphanumeric, SampleString};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncWriteExt,
    time::{Instant, sleep},
};
#[cfg(not(target_os = "windows"))]
use tokio::{
    fs::create_dir_all,
    process::{Child, Command},
};
use tracing::Level;
use uuid::Uuid;

use crate::{
    CliError, CliResult, ci::error::CiError, config::ci::*, data::UserData,
    internal_proxy::IntProxyShutdown,
};

pub(crate) mod container;
pub(crate) mod error;
pub(super) mod start;
pub(crate) mod stop;

/// Optional CI credentials for operator-backed `mirrord ci start` and `mirrord ci container`.
///
/// Supporting Free or Teams operators accept automatic ordinary credentials when this is unset.
/// CI retains the license tier's features. For Free and Teams CI, the operator limits each
/// target-routing loader to one ready pod per participating cluster, including with a supplied
/// key. Terminating pods may drain while replacements attach.
/// Enterprise and legacy installations require a key from [`generate_ci_api_key`]. The operator
/// enforces its license policy, and a supplied invalid key never falls back to ordinary
/// credentials. Without the operator, no API key is required. Local `mirrord ci stop` uses saved
/// process state and requires no operator credentials.
pub(crate) const MIRRORD_CI_API_KEY: &str = "MIRRORD_CI_API_KEY";

/// Alias for mirrord-for-ci results.
type CiResult<T> = Result<T, crate::ci::error::CiError>;

/// Try to load and decode the [`MIRRORD_CI_API_KEY`] env var.
pub(crate) fn ci_api_key_available() -> CiResult<Option<CiApiKey>> {
    match std::env::var(MIRRORD_CI_API_KEY) {
        Ok(api_key) => Ok(Some(CiApiKey::decode(&api_key)?)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(fail @ env::VarError::NotUnicode(..)) => Err(CiError::EnvVar(MIRRORD_CI_API_KEY, fail)),
    }
}

/// Handle commands related to CI `mirrord ci ...`
#[mirrord_macros::boxed_future]
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
        CiCommand::Stop => Ok(stop::CiStopCommandHandler::new(
            temp_dir().join(MirrordCiStore::SESSIONS_DIR_NAME),
            ProgressTracker::from_env("mirrord ci stop"),
        )
        .handle()
        .await?),
        CiCommand::Container(container_args) => {
            Ok(
                container::CiContainerCommandHandler::new(container_args, watch, user_data)
                    .await?
                    .handle()
                    .await?,
            )
        }
    }
}

/// Generate a new API key for CI usage by calling the operator API:
/// `POST /mirrordclusteroperatorusercredentials`
#[tracing::instrument(level = Level::TRACE, ret)]
async fn generate_ci_api_key(config_file: Option<PathBuf>) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord ci api-key");

    let mut cfg_context =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, config_file);
    let layer_config = crate::util::resolve_layer_config(&mut cfg_context)
        .await
        .inspect_err(|error| {
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

/// Sidecar container that is started as part of `mirrord ci container`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct MirrordCiManagedContainer {
    /// Container runtimes that we support (e.g. `docker`), see [`ContainerRuntime`].
    pub(crate) runtime: ContainerRuntime,

    /// Id of the container, so we can `{runtime} rm {container_id}` to kill it from `mirrord ci
    /// stop`.
    pub(crate) container_id: String,
}

/// Stores the [`MirrordCi`] information we need to execute commands like `mirrord ci start|stop`.
///
/// Each CI session has its own store, in its own directory under [`Self::SESSIONS_DIR_NAME`], so
/// the sessions that run on one machine never wait on each other, and `mirrord ci stop` can stop
/// them one at a time.
///
/// - Note that it does **not** store the [`CiApiKey`], this one lives only as an env var.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
// CI state survives across CLI invocations; missing fields must not prevent cleanup of known
// targets.
#[serde(default)]
struct MirrordCiStore {
    /// pid of the intproxy, stored when the intproxy starts.
    intproxy_pids: HashSet<u32>,

    /// pid of the extproxy, stored when `mirrord ci container` starts it.
    extproxy_pids: HashSet<u32>,

    /// pid of the sidecar runtime attach client, stored when `mirrord ci container` starts it.
    sidecar_pids: HashSet<u32>,

    /// Sidecar containers started by `mirrord ci container` that should be stopped via the
    /// container runtime.
    sidecar_containers: HashSet<MirrordCiManagedContainer>,

    /// Leader pid of each process group created for a background user command.
    ///
    /// Each background command uses `process_group(0)`, so its pid is also the process group id
    /// used to terminate its descendants.
    user_process_groups: HashSet<u32>,
}

/// Exclusive lock on the state of one CI session, held across every read-modify-write of it.
///
/// Keeping the same lock across a stop's read, signals, and ledger update prevents lock
/// contention from leaving already-signalled numeric IDs available for a later retry.
struct CiStoreLock {
    _file: fs::File,
}

impl MirrordCiStore {
    /// Directory under [`temp_dir`] that holds one directory per CI session, named after its
    /// session id.
    ///
    /// The name has no per-user part, because a CI runner runs all its jobs as one user.
    const SESSIONS_DIR_NAME: &str = "mirrord-ci";

    /// File in a session's directory where we store the intproxy pid, user process group, and
    /// whatever else we need for [`MirrordCi`].
    const STATE_FILE_NAME: &str = "session.json";

    /// File in a session's directory that the session's lock is taken on.
    ///
    /// The lock has its own file because atomic replacement changes the state file's inode, so
    /// locking the state file itself would let another writer lock the replacement.
    const LOCK_FILE_NAME: &str = "session.lock";

    /// Prefix of the temporary file that [`Self::write_locked`] writes the state to before it
    /// replaces the state file.
    ///
    /// A writer that is killed before the replacement leaves this file behind, so
    /// [`Self::remove_session`] has to recognize it to remove the session's directory.
    const TEMPORARY_STATE_FILE_PREFIX: &str = ".session.json.";

    /// How often [`Self::lock_existing_session`] retries a held lock.
    ///
    /// The processes of a session (the CLI and its intproxy) hold its lock for only a few
    /// milliseconds at a time (reading and rewriting the state file, and for a background user
    /// command also spawning it), so waiters retry often to keep the start of a session fast.
    const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(5);

    /// How long the processes of a session wait for another process to release the session's
    /// lock before they fail with [`CiError::SessionLocked`]. These processes are the CLI of
    /// `mirrord ci start` or `mirrord ci container`, and the intproxy that `mirrord ci start`
    /// starts.
    ///
    /// The processes of one session hold the lock for only a few milliseconds each, so this is
    /// generous for them. The long holder is `mirrord ci stop`. It stops every session on the
    /// machine, including one that is still starting, and holds the session's lock while it stops
    /// the session. Stopping a `mirrord ci container` session can take up to 10 seconds, which is
    /// longer than this timeout. So when a stop reaches a session that is still starting, a
    /// process of that session can fail.
    const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

    /// Directory of the session with `session_id`, under [`Self::SESSIONS_DIR_NAME`].
    fn session_dir(session_id: Uuid) -> PathBuf {
        temp_dir()
            .join(Self::SESSIONS_DIR_NAME)
            .join(session_id.to_string())
    }

    fn state_path(session_dir: &Path) -> PathBuf {
        session_dir.join(Self::STATE_FILE_NAME)
    }

    fn lock_path(session_dir: &Path) -> PathBuf {
        session_dir.join(Self::LOCK_FILE_NAME)
    }

    /// Whether `lock_file` is still the lock file at the lock path of `session_dir`.
    ///
    /// `mirrord ci stop` deletes a stopped session's lock file while it holds the lock. A process
    /// that opened the file before the deletion gets the lock once stop releases it, but holds
    /// it on a file nobody else can open any more, so it has to start over with the file now at
    /// the path.
    #[cfg(not(target_os = "windows"))]
    async fn holds_current_lock_file(session_dir: &Path, lock_file: &fs::File) -> io::Result<bool> {
        let held = lock_file.metadata().await?;
        match fs::metadata(Self::lock_path(session_dir)).await {
            Ok(current) => Ok(current.dev() == held.dev() && current.ino() == held.ino()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// `mirrord ci` doesn't run on Windows, so nothing deletes the lock file there.
    #[cfg(target_os = "windows")]
    async fn holds_current_lock_file(
        _session_dir: &Path,
        _lock_file: &fs::File,
    ) -> io::Result<bool> {
        Ok(true)
    }

    /// Takes the lock of the session in `session_dir` for one of the session's own processes (the
    /// CLI or its intproxy), creating the directory if needed, and waiting up to `lock_timeout`
    /// for another holder to release the lock.
    ///
    /// The other processes of the session hold the lock for a few milliseconds, and a
    /// `mirrord ci stop` holds it while it stops the session, so a held lock is a reason to wait
    /// rather than to fail. A stop uses [`Self::lock_existing_session`] instead, so that it never
    /// creates a session's directory.
    async fn lock_or_create_session(
        session_dir: &Path,
        lock_timeout: Duration,
    ) -> CiResult<CiStoreLock> {
        loop {
            fs::create_dir_all(session_dir).await?;
            // `None` means that `mirrord ci stop` removed the session's directory after
            // `create_dir_all` made sure that it exists, so it is created again for the state that
            // the caller is about to write.
            if let Some(lock) = Self::lock_existing_session(session_dir, lock_timeout).await? {
                return Ok(lock);
            }
        }
    }

    /// Takes the lock of the session in `session_dir`, waiting up to `lock_timeout` for another
    /// holder to release it, or returns `None` when the session's directory doesn't exist.
    ///
    /// The directory is never created here, so a `mirrord ci stop` that reaches a session that
    /// another stop has just removed finds it gone, instead of bringing back an empty one.
    ///
    /// Only the wait for a held lock counts against `lock_timeout`, not the file operations, so a
    /// zero `lock_timeout` tries the lock once and fails only if another process holds it.
    async fn lock_existing_session(
        session_dir: &Path,
        lock_timeout: Duration,
    ) -> CiResult<Option<CiStoreLock>> {
        let deadline = Instant::now() + lock_timeout;
        loop {
            let lock_file = match fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(Self::lock_path(session_dir))
                .await
            {
                Ok(lock_file) => lock_file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            while lock_file.try_lock_exclusive()?.not() {
                let now = Instant::now();
                if now >= deadline {
                    return Err(CiError::SessionLocked {
                        timeout: lock_timeout,
                    });
                }
                sleep(Self::LOCK_RETRY_INTERVAL.min(deadline - now)).await;
            }
            if Self::holds_current_lock_file(session_dir, &lock_file).await? {
                return Ok(Some(CiStoreLock { _file: lock_file }));
            }
        }
    }

    /// Reads the stored state of the session in `session_dir`, changes it with `apply`, and
    /// writes the result back.
    ///
    /// Fails with [`CiError::SessionLocked`] if the lock isn't free within `lock_timeout`.
    async fn update(
        session_dir: &Path,
        lock_timeout: Duration,
        apply: impl FnOnce(&mut Self),
    ) -> CiResult<()> {
        let lock = Self::lock_or_create_session(session_dir, lock_timeout).await?;
        Self::update_locked(session_dir, &lock, apply).await
    }

    /// [`Self::update`] for a caller that already holds the lock.
    ///
    /// The read happens under the same lock as the write. A process that read the session's state
    /// before it took the lock could write back a copy that misses the cleanup targets that
    /// another process of the session recorded in between, and `mirrord ci stop` would then leave
    /// those processes running.
    async fn update_locked(
        session_dir: &Path,
        lock: &CiStoreLock,
        apply: impl FnOnce(&mut Self),
    ) -> CiResult<()> {
        let mut store = Self::read_from_file_or_default(&Self::state_path(session_dir)).await?;
        apply(&mut store);
        store.write_locked(session_dir, lock).await
    }

    /// Replace the state only after its contents are complete. A partial write must not erase
    /// the cleanup targets from the previous successful invocation.
    async fn write_locked(&self, session_dir: &Path, _lock: &CiStoreLock) -> CiResult<()> {
        let contents = serde_json::to_vec(self)?;
        let temporary = tempfile::Builder::new()
            .prefix(Self::TEMPORARY_STATE_FILE_PREFIX)
            .tempfile_in(session_dir)?;
        let mut temporary_file = fs::File::from_std(temporary.reopen()?);
        temporary_file.write_all(&contents).await?;
        temporary_file.flush().await?;
        temporary_file.sync_all().await?;
        drop(temporary_file);
        temporary
            .persist(Self::state_path(session_dir))
            .map_err(|error| error.error)?;
        #[cfg(target_os = "linux")]
        fs::File::open(session_dir).await?.sync_all().await?;
        Ok(())
    }

    /// Tries to read the [`MirrordCiStore`] from `path` (normally the [`Self::state_path`] of a
    /// session). A missing file is an empty store; a damaged file is an error, so cleanup cannot
    /// silently report success while its recorded targets remain running.
    async fn read_from_file_or_default(path: &Path) -> CiResult<Self> {
        match fs::read(path).await {
            Ok(contents) => Ok(serde_json::from_slice(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Removes the temporary state files of the session in `session_dir`, then its state file,
    /// then its lock file, then the directory itself, and treats each one that is already gone as
    /// removed.
    ///
    /// A temporary state file is left only by a writer that was killed in [`Self::write_locked`],
    /// because a writer that finishes replaces the state file with it. While `lock` is held on
    /// the current lock file, no other process can be in [`Self::write_locked`], so every
    /// temporary state file found then is such a leftover, and would keep the directory forever.
    ///
    /// The state file goes before the lock file because, once the lock file is gone, another
    /// process can create a new lock file and lock it while `lock` is still held on the old one.
    /// That process then finds no state, rather than the targets that `lock` was held to signal.
    /// It may also have started writing in the directory, so a directory that isn't empty stays.
    async fn remove_session(session_dir: &Path, _lock: &CiStoreLock) -> CiResult<()> {
        let ignore_not_found = |result: io::Result<()>| match result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };

        let mut entries = fs::read_dir(session_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(Self::TEMPORARY_STATE_FILE_PREFIX.as_bytes())
            {
                ignore_not_found(fs::remove_file(entry.path()).await)?;
            }
        }
        ignore_not_found(fs::remove_file(Self::state_path(session_dir)).await)?;
        ignore_not_found(fs::remove_file(Self::lock_path(session_dir)).await)?;
        match fs::remove_dir(session_dir).await {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            result => Ok(result?),
        }
    }

    /// Check if the store is empty. Return `true` if no process is found.
    ///
    /// Used to avoid errors when calling `mirrord ci stop` multiple times, since that command
    /// should've `kill`ed everything already on the 1st run.
    fn is_empty(&self) -> bool {
        self.intproxy_pids.is_empty()
            && self.extproxy_pids.is_empty()
            && self.sidecar_pids.is_empty()
            && self.sidecar_containers.is_empty()
            && self.user_process_groups.is_empty()
    }
}

/// Spawns `command` as the leader of a new process group, so that `mirrord ci stop` can end it
/// together with its descendants by signalling the group, whose id is the child's pid.
#[cfg(not(target_os = "windows"))]
fn spawn_background_user_command(command: &mut Command) -> io::Result<Child> {
    command.process_group(0);
    command.spawn()
}

/// Retains explicit CI metadata independently of the optional [`CiApiKey`].
///
/// Operator-backed sessions use automatic ordinary credentials when the key is absent and the
/// operator advertises keyless CI for Free or Teams. CI retains the license tier's features; the
/// operator applies Free's target-loader pod allowance to Free and Teams CI in each participating
/// cluster. Existing keyless clients use the same capability on either tier. Missing keys are
/// rejected before credential issuance on operators without keyless CI. Supplied keys retain
/// their authentication errors, and the operator independently enforces its license policy.
///
/// `mirrord ci start` stores the process group of the background user command and the pid of our
/// intproxy so that we can stop these processes on `mirrord ci stop`.
#[derive(Debug)]
pub(super) struct MirrordCi {
    /// Overrides automatic ordinary credentials for operator-backed CI sessions.
    ci_api_key: Option<CiApiKey>,

    /// Id of this CI session, picked by [`MirrordCi::new`]. It names the session's directory under
    /// [`MirrordCiStore::SESSIONS_DIR_NAME`], and `mirrord ci start` passes it to its intproxy in
    /// `--mirrord-for-ci`.
    session_id: Uuid,

    /// Arguments for the mirrord for CI operations that involve starting something:
    /// - `mirrord ci start`
    /// - `mirrord ci container`
    ci_common_args: CiCommonArgs,
}

impl MirrordCi {
    #[cfg(not(target_os = "windows"))]
    async fn create_run_output_dir<P: Progress>(
        progress: &P,
        binary_path: &Path,
        CiConfig { output_dir, .. }: &CiConfig,
    ) -> CiResult<PathBuf> {
        let binary = binary_path
            .file_name()
            .unwrap_or(binary_path.as_os_str())
            .to_string_lossy();

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

        progress.info(&format!(
            "mirrord ci files will be stored in {}",
            ci_run_output_dir.display()
        ));

        Ok(ci_run_output_dir)
    }

    /// Spawns the user command, and records a background one in [`MirrordCiStore`] so that
    /// `mirrord ci stop` can end it.
    ///
    /// A background command is spawned only once its session's lock is held, and recorded under
    /// that same lock: if the wait for the lock timed out after the spawn, the command would keep
    /// running with no record, out of reach of `mirrord ci stop`. A foreground command ends with
    /// this session, so it takes no lock and isn't recorded.
    #[cfg(not(target_os = "windows"))]
    async fn spawn_user_command<P: Progress>(
        &self,
        progress: &mut P,
        command: &mut Command,
        binary: &str,
        binary_args: &[String],
        session_dir: &Path,
        lock_timeout: Duration,
    ) -> CiResult<Child> {
        let spawn_failed = |fail: io::Error| {
            progress.failure(Some(&fail.to_string()));
            CiError::BinaryExecuteFailed(binary.to_owned(), binary_args.to_vec())
        };

        if self.ci_common_args.foreground {
            return command.spawn().map_err(spawn_failed);
        }

        let lock = MirrordCiStore::lock_or_create_session(session_dir, lock_timeout).await?;
        // A background command inherits no descriptor for the lock file, because std opens files
        // with `O_CLOEXEC`, so the lock is released when this session drops it.
        let child = spawn_background_user_command(command).map_err(spawn_failed)?;
        if let Some(process_group) = child.id() {
            MirrordCiStore::update_locked(session_dir, &lock, |store| {
                store.user_process_groups.insert(process_group);
            })
            .await?;
        }

        Ok(child)
    }

    /// Helper to access the [`CiApiKey`] when preparing the kube request headers.
    pub(super) fn api_key(&self) -> Option<&CiApiKey> {
        self.ci_api_key.as_ref()
    }

    /// Id of this session, which `mirrord ci start` passes to its intproxy in `--mirrord-for-ci`,
    /// so that the intproxy records its pid in this session's state.
    pub(super) fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// When intproxy starts, we need to retrieve its pid and store it in the state of its session
    /// (`session_id`, from `--mirrord-for-ci`), so we can kill the intproxy later. Requiring the
    /// live shutdown owner prevents publishing the pid before its SIGTERM handler is registered.
    /// Waits up to [`MirrordCiStore::LOCK_TIMEOUT`] for the session's lock, and fails if another
    /// process holds it for longer.
    #[tracing::instrument(level = Level::TRACE, skip(_shutdown_handler), err)]
    pub(super) async fn prepare_intproxy(
        session_id: Uuid,
        _shutdown_handler: &IntProxyShutdown,
    ) -> CiResult<()> {
        MirrordCiStore::update(
            &MirrordCiStore::session_dir(session_id),
            MirrordCiStore::LOCK_TIMEOUT,
            |store| {
                store.intproxy_pids.insert(std::process::id());
            },
        )
        .await
    }

    /// Prepares and runs the user binary with mirrord.
    ///
    /// Very similar to to how `mirrord exec` behaves, except that here we `spawn` a child process
    /// that'll keep running, and for background commands we store its process group in
    /// [`MirrordCiStore`] so we can terminate it and its descendants later.
    #[cfg(not(target_os = "windows"))]
    #[tracing::instrument(level = Level::TRACE, skip(progress), err)]
    pub(super) async fn prepare_command<P: Progress>(
        self,
        progress: &mut P,
        binary_path: &Path,
        binary_args: &[String],
        env_vars: &HashMap<String, String>,
        ci_config: &CiConfig,
    ) -> CiResult<()> {
        use nix::libc::{SIGINT, SIGKILL, SIGTERM};

        let binary = binary_path
            .file_name()
            // Safety: binary path is a resolved path, and  `file_name()` only fails
            // when the last part is `..`
            .expect("failed to get file name of binary path")
            .to_string_lossy();

        let ci_run_output_dir =
            Self::create_run_output_dir(progress, binary_path, ci_config).await?;

        let mut command = resolve_tokio_command(binary_path);
        command
            .args(binary_args.iter().skip(1))
            .envs(env_vars)
            .stdin(Stdio::null())
            .stdout(File::create(ci_run_output_dir.join("stdout"))?)
            .stderr(File::create(ci_run_output_dir.join("stderr"))?)
            .kill_on_drop(false);

        let mut child = self
            .spawn_user_command(
                progress,
                &mut command,
                &binary,
                binary_args,
                &MirrordCiStore::session_dir(self.session_id),
                MirrordCiStore::LOCK_TIMEOUT,
            )
            .await?;

        let child_pid = child
            .id()
            .map(|pid| pid.to_string())
            .unwrap_or("unknown".to_owned());

        if self.ci_common_args.foreground {
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

    #[cfg(not(target_os = "windows"))]
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(level = Level::TRACE, skip(progress), err)]
    pub(super) async fn prepare_container_command<P: Progress>(
        self,
        progress: &mut P,
        binary_path: &str,
        binary_args: &[String],
        extproxy_pid: Option<u32>,
        sidecar_pid: Option<u32>,
        sidecar_container: Option<MirrordCiManagedContainer>,
        ci_config: &CiConfig,
    ) -> CiResult<i32> {
        use nix::libc::{SIGINT, SIGKILL, SIGTERM};

        let binary_path = Path::new(binary_path);
        let binary = binary_path
            .file_name()
            .expect("failed to get file name of binary path")
            .to_string_lossy();

        // external proxy and sidecar already have started, so we want to record these and be able
        // to delete them with `mirrord ci stop` even if the following code fails.
        MirrordCiStore::update(
            &MirrordCiStore::session_dir(self.session_id),
            MirrordCiStore::LOCK_TIMEOUT,
            |store| {
                store.extproxy_pids.extend(extproxy_pid);
                store.sidecar_pids.extend(sidecar_pid);
                store.sidecar_containers.extend(sidecar_container);
            },
        )
        .await?;

        let mut command = resolve_tokio_command(binary_path);
        command.args(binary_args).kill_on_drop(false);

        // If `--foreground` don't write stdio to file.
        if self.ci_common_args.foreground {
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        } else {
            let ci_output_dir =
                Self::create_run_output_dir(progress, binary_path, ci_config).await?;

            command
                .stdin(Stdio::null())
                .stdout(File::create(ci_output_dir.join("stdout"))?)
                .stderr(File::create(ci_output_dir.join("stderr"))?);
        }

        let mut child = self
            .spawn_user_command(
                progress,
                &mut command,
                &binary,
                binary_args,
                &MirrordCiStore::session_dir(self.session_id),
                MirrordCiStore::LOCK_TIMEOUT,
            )
            .await?;

        let child_pid = child
            .id()
            .map(|pid| pid.to_string())
            .unwrap_or("unknown".to_owned());

        if self.ci_common_args.foreground {
            progress.info(&format!("waiting for child with pid {child_pid}"));

            // If we don't stop progress here, it keeps looping forever when `--foreground` is used.
            progress.success(None);

            match child.wait().await {
                Ok(status) => {
                    if status.success() {
                        progress.success(None);
                        Ok::<_, CiError>(status.code().unwrap_or_default())
                    } else if let Some(signal) = status.signal() {
                        match signal {
                            SIGKILL => progress.success(Some("process killed by SIGKILL")),
                            SIGTERM => progress.success(Some("process terminated by SIGTERM")),
                            SIGINT => progress.success(Some("process interrupted by SIGINT")),
                            _ => progress
                                .failure(Some(&format!("process exited with status: {}", status))),
                        };
                        Ok::<_, CiError>(status.code().unwrap_or_default())
                    } else {
                        progress.failure(Some(&format!("process exited with status: {}", status)));
                        Ok::<_, CiError>(status.code().unwrap_or_default())
                    }
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
            Ok::<_, CiError>(0)
        }
    }

    #[cfg(target_os = "windows")]
    pub(super) async fn prepare_command<P: Progress>(
        self,
        progress: &mut P,
        binary_path: &Path,
        binary_args: &[String],
        env_vars: &HashMap<String, String>,
        ci_config: &CiConfig,
    ) -> CiResult<()> {
        unimplemented!("Not supported on windows.");
    }

    /// Reads the env var [`MIRRORD_CI_API_KEY`], and picks the id of a new CI session, to return
    /// a valid [`MirrordCi`].
    ///
    /// The id is picked before any process of the session is spawned, so that each process
    /// records itself in the session's own state.
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub(super) fn new(ci_common_args: CiCommonArgs) -> CiResult<Self> {
        let ci_api_key = ci_api_key_available()?;

        Ok(Self {
            ci_api_key,
            session_id: Uuid::new_v4(),
            ci_common_args,
        })
    }

    /// Converts a [`CiInfo`] into a [`SessionCiInfo`] used by the operator.
    pub(super) fn info(&self) -> SessionCiInfo {
        let CiInfo { name, .. } = ci_info::get();

        let CiCommonArgs {
            foreground: _,
            environment,
            pipeline,
            triggered_by,
        } = self.ci_common_args.clone();

        SessionCiInfo {
            provider: name,
            environment,
            pipeline,
            triggered_by,
        }
    }

    pub(super) fn is_foreground(&self) -> bool {
        self.ci_common_args.foreground
    }

    /// Whether `mirrord ci start` runs the program as a child instead of replacing itself with it
    /// (`execve`): it runs in the background, or the program's output goes to files.
    pub(super) fn spawns_the_program(&self, config: &CiConfig) -> bool {
        self.is_foreground().not() || config.output_dir.is_some()
    }
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use std::{
        collections::HashSet, ops::Not, os::unix::process::ExitStatusExt, process::Stdio,
        time::Duration,
    };

    use mirrord_command::resolve_tokio_command;
    use mirrord_progress::ProgressTracker;
    use nix::{
        sys::signal::{Signal, killpg},
        unistd::{Pid, getpgid},
    };
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        time::timeout,
    };
    use uuid::Uuid;

    use super::{CiCommonArgs, CiError, MirrordCi, MirrordCiStore, spawn_background_user_command};

    const LOCK_TIMEOUT: Duration = Duration::from_secs(1);

    #[test]
    fn ci_store_defaults_missing_fields() {
        let store: MirrordCiStore = serde_json::from_str(r#"{"intproxy_pids":[1234]}"#).unwrap();

        assert_eq!(store.intproxy_pids, HashSet::from([1234]));
        assert!(store.user_process_groups.is_empty());
    }

    #[tokio::test]
    async fn ci_store_rejects_invalid_files_and_replaces_complete_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();
        let path = MirrordCiStore::state_path(session_dir);

        assert!(
            MirrordCiStore::read_from_file_or_default(&path)
                .await
                .unwrap()
                .is_empty()
        );

        for contents in ["", "{\"intproxy_pids\":"] {
            tokio::fs::write(&path, contents).await.unwrap();
            assert!(
                MirrordCiStore::read_from_file_or_default(&path)
                    .await
                    .is_err()
            );
            assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), contents);
            assert!(
                MirrordCiStore::update(session_dir, LOCK_TIMEOUT, |_| {})
                    .await
                    .is_err()
            );
            assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), contents);
        }

        // `MirrordCiStore::update` reads before it writes, so it rejects the damaged file; start
        // from a missing one.
        tokio::fs::remove_file(&path).await.unwrap();
        MirrordCiStore::update(session_dir, LOCK_TIMEOUT, |store| {
            store.intproxy_pids.insert(1234);
        })
        .await
        .unwrap();
        assert_eq!(
            MirrordCiStore::read_from_file_or_default(&path)
                .await
                .unwrap()
                .intproxy_pids,
            HashSet::from([1234])
        );

        MirrordCiStore::update(session_dir, LOCK_TIMEOUT, |store| {
            store.intproxy_pids.clear()
        })
        .await
        .unwrap();
        assert!(
            MirrordCiStore::read_from_file_or_default(&path)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn ci_store_contested_write_preserves_existing_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();
        MirrordCiStore::update(session_dir, LOCK_TIMEOUT, |store| {
            store.intproxy_pids.insert(1234);
        })
        .await
        .unwrap();

        let lock = MirrordCiStore::lock_or_create_session(session_dir, Duration::ZERO)
            .await
            .unwrap();
        assert!(matches!(
            MirrordCiStore::update(session_dir, Duration::from_millis(100), |store| {
                *store = MirrordCiStore::default()
            })
            .await,
            Err(CiError::SessionLocked { .. })
        ));
        assert_eq!(
            MirrordCiStore::read_from_file_or_default(&MirrordCiStore::state_path(session_dir))
                .await
                .unwrap()
                .intproxy_pids,
            HashSet::from([1234])
        );
        drop(lock);
    }

    /// The processes of one session (the CLI and its intproxy) contend for its lock; the later one
    /// must wait for it and keep the targets the earlier one recorded.
    #[tokio::test]
    async fn ci_store_update_waits_for_lock_and_keeps_existing_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path().to_owned();

        let lock = MirrordCiStore::lock_or_create_session(&session_dir, Duration::ZERO)
            .await
            .unwrap();
        let holder_dir = session_dir.clone();
        let holder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut store = MirrordCiStore::default();
            store.intproxy_pids.insert(1234);
            store.write_locked(&holder_dir, &lock).await.unwrap();
            drop(lock);
        });

        MirrordCiStore::update(&session_dir, LOCK_TIMEOUT, |store| {
            store.intproxy_pids.insert(5678);
        })
        .await
        .unwrap();
        holder.await.unwrap();

        assert_eq!(
            MirrordCiStore::read_from_file_or_default(&MirrordCiStore::state_path(&session_dir))
                .await
                .unwrap()
                .intproxy_pids,
            HashSet::from([1234, 5678])
        );
    }

    /// A background user command runs only once the CLI holds its session's lock, so a lock
    /// timeout can't leave it running with no record for `mirrord ci stop`. A foreground command
    /// ends with its session, so it neither waits for the lock nor is recorded.
    #[tokio::test]
    async fn ci_user_command_background_spawns_only_under_lock() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();
        let state_path = MirrordCiStore::state_path(session_dir);
        let sentinel = temp_dir.path().join("sentinel");
        let script = format!("touch '{}'", sentinel.display());
        let user_command = || {
            let mut command = resolve_tokio_command("sh");
            command.args(["-c", &script]).kill_on_drop(true);
            command
        };
        let mut progress = ProgressTracker::null();
        let background = MirrordCi {
            ci_api_key: None,
            session_id: Uuid::new_v4(),
            ci_common_args: CiCommonArgs::default(),
        };
        let foreground = MirrordCi {
            ci_api_key: None,
            session_id: Uuid::new_v4(),
            ci_common_args: CiCommonArgs {
                foreground: true,
                ..Default::default()
            },
        };

        let lock = MirrordCiStore::lock_or_create_session(session_dir, Duration::ZERO)
            .await
            .unwrap();
        let timed_out = background
            .spawn_user_command(
                &mut progress,
                &mut user_command(),
                "sh",
                &[],
                session_dir,
                Duration::from_millis(100),
            )
            .await;
        assert!(matches!(timed_out, Err(CiError::SessionLocked { .. })));
        // A command spawned before the timeout would have had this long to create the sentinel.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(sentinel.exists().not());

        let mut foreground_child = foreground
            .spawn_user_command(
                &mut progress,
                &mut user_command(),
                "sh",
                &[],
                session_dir,
                Duration::from_millis(100),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), foreground_child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            MirrordCiStore::read_from_file_or_default(&state_path)
                .await
                .unwrap()
                .is_empty()
        );
        drop(lock);

        let mut background_child = background
            .spawn_user_command(
                &mut progress,
                &mut user_command(),
                "sh",
                &[],
                session_dir,
                LOCK_TIMEOUT,
            )
            .await
            .unwrap();
        let background_pid = background_child.id().expect("spawned child has a pid");
        assert_eq!(
            MirrordCiStore::read_from_file_or_default(&state_path)
                .await
                .unwrap()
                .user_process_groups,
            HashSet::from([background_pid])
        );
        timeout(Duration::from_secs(5), background_child.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn ci_process_group_background_spawn_leads_group_containing_descendant() {
        let mut command = resolve_tokio_command("sh");
        command
            .args(["-c", "sh -c 'sleep 30 & echo $!; wait' & wait"])
            .stdout(Stdio::piped())
            .kill_on_drop(true);

        let mut child = spawn_background_user_command(&mut command).unwrap();
        let child_pid = child.id().expect("spawned child has a pid");
        let process_group = Pid::from_raw(child_pid as i32);
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut descendant_pid = String::new();
        timeout(
            Duration::from_secs(5),
            BufReader::new(stdout).read_line(&mut descendant_pid),
        )
        .await
        .expect("descendant announces readiness")
        .unwrap();
        let descendant_pid = Pid::from_raw(descendant_pid.trim().parse().unwrap());

        assert_eq!(getpgid(Some(process_group)).unwrap(), process_group);
        assert_eq!(getpgid(Some(descendant_pid)).unwrap(), process_group);

        killpg(process_group, Signal::SIGKILL).unwrap();
        let status = timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("child exits after SIGKILL")
            .unwrap();
        assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
    }
}
