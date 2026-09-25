#![cfg_attr(windows, allow(unused))]
use std::{collections::HashSet, path::PathBuf, time::Duration};

#[cfg(not(target_os = "windows"))]
use futures::{StreamExt, stream};
use itertools::Itertools;
use mirrord_progress::{Progress, ProgressTracker};
#[cfg(not(target_os = "windows"))]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};
use tokio::process::Command;
use tracing::Level;

use super::CiResult;
#[cfg(not(target_os = "windows"))]
use crate::ci::CiError;
use crate::ci::MirrordCiStore;

/// How long every target has to react to `SIGTERM` before `mirrord ci stop` forces it down with
/// `SIGKILL`.
///
/// Waited once for the whole batch: cleanup never probes whether a target is still alive, since
/// an existence check races with pid reuse and only saves latency. The wait must stay comfortably
/// longer than the grace an intproxy uses for its own registered processes, so a CI intproxy can
/// finish terminating the layers it knows about before we kill it.
pub(super) const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Something `mirrord ci stop` has to terminate.
///
/// The distinction matters because the processes we start ourselves are single processes we know
/// the pid of, while a user command is only reachable as a whole process group: a wrapper such as
/// `npm` exits without taking down the server child that actually holds the port.
#[cfg(not(target_os = "windows"))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TerminationTarget {
    /// Signaled with [`kill`], used for the proxies and sidecars we spawn ourselves.
    Process(Pid),

    /// Signaled with [`killpg`], used for the groups created for user commands, so that the
    /// descendants of a wrapper process are terminated as well.
    ProcessGroup(Pid),
}

#[cfg(not(target_os = "windows"))]
impl TerminationTarget {
    fn process(pid: u32) -> Self {
        Self::Process(Pid::from_raw(pid as i32))
    }

    fn process_group(process_group: u32) -> Self {
        Self::ProcessGroup(Pid::from_raw(process_group as i32))
    }

    /// Sends `signal` to this target.
    ///
    /// `ESRCH` is a success: the target is already gone, which is exactly the state cleanup wants.
    /// Every other error is reported, as it means we might be leaving a process behind.
    fn signal(self, signal: Signal) -> CiResult<()> {
        let signaled = match self {
            Self::Process(pid) => kill(pid, signal),
            Self::ProcessGroup(process_group) => killpg(process_group, signal),
        };

        match signaled {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(CiError::from(error)),
        }
    }
}

/// Terminates every target, giving them `grace` to exit on their own first.
///
/// `SIGTERM` goes to all targets, then we wait `grace` once for the whole batch, then `SIGKILL`
/// goes to the same targets unconditionally. Signaling a target that already exited is harmless,
/// so nothing is checked in between: a single flat wait is simpler than per-target liveness polling
/// and ends in the same state.
///
/// Errors are collected instead of returned, so that one target we are not allowed to signal
/// doesn't leave the remaining ones running.
#[cfg(not(target_os = "windows"))]
async fn terminate_targets(
    targets: HashSet<TerminationTarget>,
    grace: Duration,
) -> Vec<CiResult<()>> {
    if targets.is_empty() {
        return Vec::new();
    }

    let mut results = targets
        .iter()
        .map(|target| target.signal(Signal::SIGTERM))
        .collect::<Vec<_>>();

    tokio::time::sleep(grace).await;

    results.extend(targets.iter().map(|target| target.signal(Signal::SIGKILL)));

    results
}

#[cfg(not(target_os = "windows"))]
impl MirrordCiStore {
    /// Cleanup consumes the stored targets; the state file is removed only after cleanup succeeds,
    /// so a failed `mirrord ci stop` can be retried.
    async fn terminate(self, grace: Duration) -> CiResult<()> {
        let MirrordCiStore {
            intproxy_pids,
            extproxy_pids,
            sidecar_pids,
            sidecar_containers,
            user_process_groups,
        } = self;

        let targets = intproxy_pids
            .into_iter()
            .chain(extproxy_pids)
            .chain(sidecar_pids)
            .map(TerminationTarget::process)
            .chain(
                user_process_groups
                    .into_iter()
                    .map(TerminationTarget::process_group),
            )
            .collect::<HashSet<_>>();

        let targets_terminated = terminate_targets(targets, grace).await;

        let sidecars_removed = stream::iter(sidecar_containers)
            .then(runtime_remove_container)
            .collect::<Vec<_>>()
            .await;

        targets_terminated
            .into_iter()
            .try_collect::<_, (), _>()
            .and(sidecars_removed.into_iter().try_collect::<_, (), _>())
    }
}

/// Kills the sidecars that were started by `mirrord ci container`.
///
/// When running `mirrord ci container`, the `intproxy` is started as `root`, so a regular user
/// won't be able to kill it with `mirrord ci stop`, and thus we need to use something
/// like `docker rm` to stop it.
#[cfg(not(target_os = "windows"))]
async fn runtime_remove_container(container: crate::ci::MirrordCiManagedContainer) -> CiResult<()> {
    let runtime = container.runtime.command();
    let command = format!("{runtime} rm -f {}", container.container_id);

    let output = Command::new(runtime)
        .args(["rm", "-f", container.container_id.as_str()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|error| CiError::ContainerRuntimeCommand {
            command: command.clone(),
            message: error.to_string(),
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_lowercase();

    // No need to warn the user on anything if the container doesn't exist.
    if stderr.contains("no such") || stderr.contains("not found") {
        Ok(())
    } else {
        Err(CiError::ContainerRuntimeCommand {
            command,
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Handles the `mirrord ci stop` command.
///
/// Builds a [`MirrordCiStore`] to kill the intproxy and the user's binary that was started by
/// `mirrord ci start`.
pub(super) struct CiStopCommandHandler {
    /// The [`MirrordCiStore`] we retrieve from the user's environment (env var and temp files) so
    /// we can kill the intproxy and the user's process.
    pub(crate) store: MirrordCiStore,

    progress: ProgressTracker,
    store_path: PathBuf,
    grace: Duration,
}

impl CiStopCommandHandler {
    /// Loads the CI state from `store_path` for cleanup.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(
        store_path: PathBuf,
        grace: Duration,
        progress: ProgressTracker,
    ) -> CiResult<Self> {
        let store = MirrordCiStore::read_from_file_or_default(&store_path).await?;

        Ok(Self {
            store,
            progress,
            store_path,
            grace,
        })
    }

    /// Terminates the processes recorded in [`MirrordCiStore`], and removes the state file.
    ///
    /// The recorded user process groups are signaled as groups, so that descendants of a wrapper
    /// command (the server an `npm start` left behind, for instance) are terminated too.
    #[cfg(not(target_os = "windows"))]
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(super) async fn handle(self) -> CiResult<()> {
        let Self {
            store,
            mut progress,
            store_path,
            grace,
        } = self;

        // If `ci stop` is issued multiple time, we should exit with success status.
        if store.is_empty() {
            progress.success(Some(
                "No mirrord ci processes found. \
                You can also manually stop mirrord by searching for the pids with \
                `ps | grep mirrord` and calling `kill [pid]`.
                ",
            ));
            return Ok(());
        }

        store.terminate(grace).await?;

        MirrordCiStore::remove_file(&store_path).await?;
        progress.success(None);

        Ok(())
    }

    #[cfg_attr(windows, allow(unused))]
    #[cfg(target_os = "windows")]
    pub(super) async fn handle(self) -> CiResult<()> {
        unimplemented!("Command not supported on windows.");
    }
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use std::{
        collections::HashSet, ops::Not, os::unix::process::ExitStatusExt, process::Stdio,
        time::Duration,
    };

    use mirrord_progress::ProgressTracker;
    use nix::{
        errno::Errno,
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::{Child, ChildStdout, Command},
        time::{Instant, timeout},
    };

    use super::CiStopCommandHandler;
    use crate::ci::{MirrordCiStore, spawn_background_user_command};

    /// Short stand-in for [`super::SHUTDOWN_GRACE`], the production value would only make the
    /// tests slow.
    const TEST_GRACE: Duration = Duration::from_millis(300);

    /// Spawns `script` the same way a CI user command is spawned, recording its process group in
    /// `store`, and waits for the script to announce itself with a `ready` line.
    ///
    /// Waiting for that line instead of sleeping keeps the tests from signaling a shell that has
    /// not installed its traps yet.
    async fn spawn_ready_script(
        script: &str,
        store: &mut MirrordCiStore,
    ) -> (Child, BufReader<ChildStdout>) {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true);

        let mut child =
            spawn_background_user_command(&mut command, store).expect("failed to spawn script");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut ready = String::new();
        timeout(Duration::from_secs(5), stdout.read_line(&mut ready))
            .await
            .expect("script announces readiness")
            .unwrap();
        assert_eq!(ready, "ready\n");

        (child, stdout)
    }

    /// Polls until `pid` is gone, so that the assertions don't race with the kernel reaping a
    /// process we just signaled.
    async fn assert_gone(pid: Pid) {
        timeout(Duration::from_secs(5), async {
            while kill(pid, None) != Err(Errno::ESRCH) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("process {pid} is still running"));
    }

    /// The regression this feature exists for: a wrapper command must not leave its children
    /// behind, which only works because we signal the whole process group.
    #[tokio::test]
    async fn ci_stop_terminates_user_process_group_descendants() {
        let mut store = MirrordCiStore::default();
        let (mut child, mut stdout) = spawn_ready_script(
            "sh -c 'while :; do sleep 1; done' & echo ready; echo $!; wait",
            &mut store,
        )
        .await;

        let mut grandchild_pid = String::new();
        timeout(
            Duration::from_secs(5),
            stdout.read_line(&mut grandchild_pid),
        )
        .await
        .expect("script reports its child pid")
        .unwrap();
        let grandchild_pid = Pid::from_raw(grandchild_pid.trim().parse::<i32>().unwrap());

        store.terminate(TEST_GRACE).await.unwrap();

        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("wrapper exits")
            .unwrap();
        assert_gone(grandchild_pid).await;
    }

    /// `SIGTERM` comes first so that a well behaved process can run its own cleanup, which is the
    /// whole point of not going straight to `SIGKILL`.
    #[tokio::test]
    async fn ci_stop_lets_targets_handle_sigterm() {
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap 'exit 7' TERM; echo ready; while :; do :; done",
            &mut store,
        )
        .await;

        store.terminate(TEST_GRACE).await.unwrap();

        let status = timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("script exits through its TERM trap")
            .unwrap();
        assert_eq!(status.code(), Some(7));
    }

    /// A process that ignores `SIGTERM` must still be gone when `mirrord ci stop` returns, so the
    /// escalation to `SIGKILL` happens even though nothing checked whether it was still alive.
    #[tokio::test]
    async fn ci_stop_escalates_to_sigkill() {
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap '' TERM; echo ready; while :; do sleep 1; done",
            &mut store,
        )
        .await;

        let shutdown_started = Instant::now();
        store.terminate(TEST_GRACE).await.unwrap();

        let status = timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("script is killed after the grace period")
            .unwrap();
        assert!(shutdown_started.elapsed() >= TEST_GRACE);
        assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
    }

    /// Targets recorded in the state file are routinely gone by the time `mirrord ci stop` runs
    /// (a CI job that ended on its own), and that is a successful cleanup, not a failure.
    #[tokio::test]
    async fn ci_stop_succeeds_for_already_exited_targets() {
        let mut store = MirrordCiStore::default();
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]).kill_on_drop(true);
        let mut child =
            spawn_background_user_command(&mut command, &mut store).expect("failed to spawn");
        let pid = child.id().expect("spawned child has a pid");
        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("script exits on its own")
            .unwrap();

        store.intproxy_pids = HashSet::from([pid]);

        store.terminate(TEST_GRACE).await.unwrap();
    }

    /// The second invocation must load the absence left by the first invocation, rather than reuse
    /// an already-empty in-memory store. CI cleanup commonly invokes stop unconditionally, so both
    /// the deletion and the load-from-absent path are part of its idempotency contract.
    #[tokio::test]
    async fn ci_stop_removes_persisted_state_and_second_stop_succeeds() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store_path = temp_dir.path().join("mirrord-for-ci.json");
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap 'exit 0' TERM; echo ready; while :; do :; done",
            &mut store,
        )
        .await;
        assert!(store.is_empty().not());
        tokio::fs::write(&store_path, serde_json::to_vec(&store).unwrap())
            .await
            .unwrap();

        let first =
            CiStopCommandHandler::new(store_path.clone(), TEST_GRACE, ProgressTracker::null())
                .await
                .unwrap();
        assert!(first.store.is_empty().not());
        first.handle().await.unwrap();

        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("persisted target exits")
            .unwrap();
        assert!(store_path.exists().not());

        let second =
            CiStopCommandHandler::new(store_path.clone(), TEST_GRACE, ProgressTracker::null())
                .await
                .unwrap();
        assert!(second.store.is_empty());
        second.handle().await.unwrap();
        assert!(store_path.exists().not());
    }
}
