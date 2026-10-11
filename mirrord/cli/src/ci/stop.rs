#![cfg_attr(windows, allow(unused))]
#[cfg(not(target_os = "windows"))]
use std::future::Future;
use std::{collections::HashSet, path::PathBuf};

#[cfg(not(target_os = "windows"))]
use futures::{StreamExt, stream};
use mirrord_command::resolve_tokio_command;
use mirrord_progress::{Progress, ProgressTracker};
#[cfg(not(target_os = "windows"))]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};
use tracing::Level;

use super::CiResult;
#[cfg(not(target_os = "windows"))]
use crate::ci::CiError;
use crate::ci::{CiStoreLock, MirrordCiManagedContainer, MirrordCiStore};

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

/// Asks intproxy to shut down its registered layers, and kills other targets once.
///
/// Intproxy completes registration quiescence after receiving `SIGTERM`; `ci stop` does not wait
/// for it to exit. Other numeric targets are not signalled again because their IDs could be reused.
/// Errors are collected so one inaccessible target does not prevent cleanup of the rest.
#[cfg(not(target_os = "windows"))]
fn terminate_targets(
    intproxy_targets: HashSet<TerminationTarget>,
    other_targets: HashSet<TerminationTarget>,
) -> Vec<(TerminationTarget, CiResult<()>)> {
    intproxy_targets
        .into_iter()
        .map(|target| (target, target.signal(Signal::SIGTERM)))
        .chain(
            other_targets
                .into_iter()
                .map(|target| (target, target.signal(Signal::SIGKILL))),
        )
        .collect()
}

#[cfg(not(target_os = "windows"))]
impl MirrordCiStore {
    /// Successful signal attempts are removed from the retry ledger so a later stop cannot
    /// re-signal their numeric IDs merely because an unrelated cleanup operation failed.
    fn record_signal_results(
        &mut self,
        results: Vec<(TerminationTarget, CiResult<()>)>,
    ) -> Option<CiError> {
        let mut first_error = None;
        for (target, result) in results {
            match result {
                Ok(()) => match target {
                    TerminationTarget::Process(pid) => {
                        let pid = pid.as_raw() as u32;
                        self.intproxy_pids.remove(&pid);
                        self.extproxy_pids.remove(&pid);
                        self.sidecar_pids.remove(&pid);
                    }
                    TerminationTarget::ProcessGroup(pgid) => {
                        self.user_process_groups.remove(&(pgid.as_raw() as u32));
                    }
                },
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error
    }

    /// Attempts every cleanup action even if one fails, leaving only failed targets for retry.
    async fn terminate_with<Remove, Removal>(
        &mut self,
        mut remove_container: Remove,
    ) -> CiResult<()>
    where
        Remove: FnMut(MirrordCiManagedContainer) -> Removal,
        Removal: Future<Output = CiResult<()>>,
    {
        let intproxy_targets = self
            .intproxy_pids
            .iter()
            .copied()
            .map(TerminationTarget::process)
            .collect::<HashSet<_>>();
        let mut other_targets = self
            .extproxy_pids
            .iter()
            .chain(&self.sidecar_pids)
            .copied()
            .map(TerminationTarget::process)
            .chain(
                self.user_process_groups
                    .iter()
                    .copied()
                    .map(TerminationTarget::process_group),
            )
            .collect::<HashSet<_>>();
        // A PID recorded under two roles still belongs to intproxy's graceful shutdown path.
        other_targets.retain(|target| !intproxy_targets.contains(target));

        let mut first_error =
            self.record_signal_results(terminate_targets(intproxy_targets, other_targets));
        let containers = std::mem::take(&mut self.sidecar_containers);
        let removals = stream::iter(containers)
            .then(|container| {
                let removal = remove_container(container.clone());
                async move { (container, removal.await) }
            })
            .collect::<Vec<_>>()
            .await;
        for (container, result) in removals {
            if let Err(error) = result {
                self.sidecar_containers.insert(container);
                first_error.get_or_insert(error);
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
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

    let output = resolve_tokio_command(runtime)
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
    lock: CiStoreLock,
}

impl CiStopCommandHandler {
    /// Locks before loading state so a contested writer cannot leave already-signalled targets
    /// in the persisted ledger after cleanup.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(store_path: PathBuf, progress: ProgressTracker) -> CiResult<Self> {
        let lock = MirrordCiStore::lock_file(&store_path).await?;
        let store = MirrordCiStore::read_from_file_or_default(&store_path).await?;

        Ok(Self {
            store,
            progress,
            store_path,
            lock,
        })
    }

    /// Terminates the processes recorded in [`MirrordCiStore`], and removes the state file.
    ///
    /// The recorded user process groups are signaled as groups, so that descendants of a wrapper
    /// command (the server an `npm start` left behind, for instance) are terminated too.
    #[cfg(not(target_os = "windows"))]
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(super) async fn handle(self) -> CiResult<()> {
        self.handle_with(runtime_remove_container).await
    }

    #[cfg(not(target_os = "windows"))]
    async fn handle_with<Remove, Removal>(self, remove_container: Remove) -> CiResult<()>
    where
        Remove: FnMut(MirrordCiManagedContainer) -> Removal,
        Removal: Future<Output = CiResult<()>>,
    {
        let Self {
            mut store,
            mut progress,
            store_path,
            lock,
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

        let cleanup_result = store.terminate_with(remove_container).await;
        if store.is_empty() {
            MirrordCiStore::remove_file(&store_path, &lock).await?;
        } else {
            store.write_locked(&store_path, &lock).await?;
        }
        cleanup_result?;
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

    use mirrord_command::resolve_tokio_command;
    use mirrord_config::container::ContainerRuntime;
    use mirrord_progress::ProgressTracker;
    #[cfg(not(target_os = "linux"))]
    use nix::sys::signal::kill;
    use nix::{errno::Errno, sys::signal::Signal, unistd::Pid};
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::{Child, ChildStdout},
        time::timeout,
    };

    use super::{CiStopCommandHandler, TerminationTarget, runtime_remove_container};
    use crate::ci::{
        CiError, MirrordCiManagedContainer, MirrordCiStore, spawn_background_user_command,
    };

    /// Spawns `script` the same way a CI user command is spawned, recording its process group in
    /// `store`, and waits for the script to announce itself with a `ready` line.
    ///
    /// Waiting for that line instead of sleeping keeps the tests from signaling a shell that has
    /// not installed its traps yet.
    async fn spawn_ready_script(
        script: &str,
        store: &mut MirrordCiStore,
    ) -> (Child, BufReader<ChildStdout>) {
        let mut command = resolve_tokio_command("sh");
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

    /// Waits until the child has exited. In Linux CI containers, PID 1 may not reap an orphaned
    /// child promptly, so a zombie still has a PID even though it cannot run or hold a port.
    async fn assert_gone(pid: Pid) {
        timeout(Duration::from_secs(5), async {
            loop {
                #[cfg(target_os = "linux")]
                let exited = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                    Ok(stat) => stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, fields)| fields.starts_with("Z ")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                    // A process that disappears between opening and reading its procfs stat file
                    // can yield ESRCH.
                    Err(error) if error.raw_os_error() == Some(Errno::ESRCH as i32) => true,
                    Err(error) => panic!("failed to inspect process {pid}: {error}"),
                };
                #[cfg(not(target_os = "linux"))]
                let exited = kill(pid, None) == Err(Errno::ESRCH);

                if exited {
                    break;
                }
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

        store
            .terminate_with(runtime_remove_container)
            .await
            .unwrap();

        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("wrapper exits")
            .unwrap();
        assert_gone(grandchild_pid).await;
    }

    /// Intproxy needs `SIGTERM` to run its registration barrier rather than being killed like a
    /// background user command.
    #[tokio::test]
    async fn ci_stop_lets_intproxy_handle_sigterm() {
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap 'exit 7' TERM; echo ready; while :; do :; done",
            &mut store,
        )
        .await;
        store.user_process_groups.clear();
        store.intproxy_pids.insert(child.id().unwrap());
        store.extproxy_pids.insert(child.id().unwrap());

        store
            .terminate_with(runtime_remove_container)
            .await
            .unwrap();

        let status = timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("script exits through its TERM trap")
            .unwrap();
        assert_eq!(status.code(), Some(7));
    }

    /// User process groups get one immediate kill, even if intproxy keeps shutting down afterward.
    #[tokio::test]
    async fn ci_stop_kills_user_group_without_sigterm() {
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap 'exit 7' TERM; echo ready; while :; do sleep 1; done",
            &mut store,
        )
        .await;

        store
            .terminate_with(runtime_remove_container)
            .await
            .unwrap();

        let status = timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("script is killed")
            .unwrap();
        assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
    }

    /// Other directly managed processes get only one kill, even if they would handle TERM.
    #[tokio::test]
    async fn ci_stop_kills_managed_process_without_sigterm() {
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap 'exit 7' TERM; echo ready; while :; do :; done",
            &mut store,
        )
        .await;
        store.user_process_groups.clear();
        store.extproxy_pids.insert(child.id().unwrap());

        store
            .terminate_with(runtime_remove_container)
            .await
            .unwrap();

        let status = timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("managed process is killed")
            .unwrap();
        assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
    }

    /// Targets recorded in the state file are routinely gone by the time `mirrord ci stop` runs
    /// (a CI job that ended on its own), and that is a successful cleanup, not a failure.
    #[tokio::test]
    async fn ci_stop_succeeds_for_already_exited_targets() {
        let mut store = MirrordCiStore::default();
        let mut command = resolve_tokio_command("sh");
        command.args(["-c", "exit 0"]).kill_on_drop(true);
        let mut child =
            spawn_background_user_command(&mut command, &mut store).expect("failed to spawn");
        let pid = child.id().expect("spawned child has a pid");
        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("script exits on its own")
            .unwrap();

        store.intproxy_pids = HashSet::from([pid]);

        store
            .terminate_with(runtime_remove_container)
            .await
            .unwrap();
    }

    /// CI stop dispatches its signals without waiting for intproxy to complete shutdown. Its own
    /// watchdog is responsible for a proxy that stalls after receiving SIGTERM.
    #[tokio::test]
    async fn ci_stop_does_not_wait_for_intproxy_to_exit() {
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) =
            spawn_ready_script("trap '' TERM; echo ready; while :; do :; done", &mut store).await;
        store.user_process_groups.clear();
        store.intproxy_pids.insert(child.id().unwrap());

        timeout(
            Duration::from_secs(1),
            store.terminate_with(runtime_remove_container),
        )
        .await
        .expect("ci stop must not wait for intproxy's shutdown")
        .unwrap();
        assert!(child.try_wait().unwrap().is_none());

        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }

    /// Contention must fail before signalling; a failed state update after signalling would leave
    /// stale numeric IDs on disk for a later stop to target again.
    #[tokio::test]
    async fn contested_state_lock_prevents_stop_from_signalling() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store_path = temp_dir.path().join("mirrord-for-ci.json");
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) =
            spawn_ready_script("echo ready; while :; do :; done", &mut store).await;
        store.write_to_path(&store_path).await.unwrap();

        let lock = MirrordCiStore::lock_file(&store_path).await.unwrap();
        let attempted_stop =
            CiStopCommandHandler::new(store_path.clone(), ProgressTracker::null()).await;
        assert!(matches!(
            attempted_stop,
            Err(CiError::IO(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(child.try_wait().unwrap().is_none());
        assert_eq!(
            MirrordCiStore::read_from_file_or_default(&store_path)
                .await
                .unwrap()
                .user_process_groups,
            store.user_process_groups
        );
        drop(lock);

        let stop = CiStopCommandHandler::new(store_path.clone(), ProgressTracker::null())
            .await
            .unwrap();
        assert!(store.write_to_path(&store_path).await.is_err());
        stop.handle().await.unwrap();
        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("group exits after successful stop")
            .unwrap();
        assert!(!store_path.exists());
    }

    /// A failed container removal must not leave an already-signalled group in the retry ledger.
    #[tokio::test]
    async fn partial_failure_retries_only_failed_container() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store_path = temp_dir.path().join("mirrord-for-ci.json");
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) =
            spawn_ready_script("echo ready; while :; do :; done", &mut store).await;
        let pgid = child.id().unwrap();
        let container = MirrordCiManagedContainer {
            runtime: ContainerRuntime::Podman,
            container_id: "test-container".to_owned(),
        };
        store.sidecar_containers.insert(container.clone());
        store.write_to_path(&store_path).await.unwrap();

        let first = CiStopCommandHandler::new(store_path.clone(), ProgressTracker::null())
            .await
            .unwrap();
        let error = first
            .handle_with(|_| async {
                Err(CiError::IO(std::io::Error::other("runtime unavailable")))
            })
            .await
            .unwrap_err();
        assert!(matches!(error, CiError::IO(_)));
        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("the user group was signalled")
            .unwrap();

        let retry = CiStopCommandHandler::new(store_path.clone(), ProgressTracker::null())
            .await
            .unwrap();
        assert!(retry.store.user_process_groups.is_empty());
        assert_eq!(retry.store.sidecar_containers, HashSet::from([container]));
        assert!(!retry.store.user_process_groups.contains(&pgid));
        retry.handle_with(|_| async { Ok(()) }).await.unwrap();
        assert!(!store_path.exists());
    }

    /// Errors on one process retain that PID, while successful signals remove every role for
    /// the signalled numeric process. Groups are tracked separately from process PIDs.
    #[test]
    fn partial_signal_failure_retains_only_failed_process() {
        let mut store = MirrordCiStore::default();
        store.intproxy_pids.insert(111);
        store.extproxy_pids.insert(111);
        store.sidecar_pids.insert(222);
        store.user_process_groups.insert(333);
        let error = store.record_signal_results(vec![
            (TerminationTarget::process(111), Ok(())),
            (
                TerminationTarget::process(222),
                Err(CiError::IO(std::io::Error::other("denied"))),
            ),
            (TerminationTarget::process_group(333), Ok(())),
        ]);
        assert!(matches!(error, Some(CiError::IO(_))));
        assert!(store.intproxy_pids.is_empty());
        assert!(store.extproxy_pids.is_empty());
        assert_eq!(store.sidecar_pids, HashSet::from([222]));
        assert!(store.user_process_groups.is_empty());
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

        let first = CiStopCommandHandler::new(store_path.clone(), ProgressTracker::null())
            .await
            .unwrap();
        assert!(first.store.is_empty().not());
        first.handle().await.unwrap();

        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("persisted target exits")
            .unwrap();
        assert!(store_path.exists().not());

        let second = CiStopCommandHandler::new(store_path.clone(), ProgressTracker::null())
            .await
            .unwrap();
        assert!(second.store.is_empty());
        second.handle().await.unwrap();
        assert!(store_path.exists().not());
    }
}
