#![cfg_attr(windows, allow(unused))]
#[cfg(not(target_os = "windows"))]
use std::future::Future;
use std::{
    collections::HashSet,
    ops::Not,
    path::{Path, PathBuf},
    time::Duration,
};

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
use tokio::fs;
use tracing::Level;

use super::CiResult;
#[cfg(not(target_os = "windows"))]
use crate::ci::CiError;
use crate::ci::{MirrordCiManagedContainer, MirrordCiStore};

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
/// Stops every CI session recorded under [`MirrordCiStore::sessions_dir`], killing the intproxy,
/// the user's binary, and whatever else `mirrord ci start` or `mirrord ci container` started.
pub(super) struct CiStopCommandHandler {
    progress: ProgressTracker,

    /// Holds one directory per CI session, see [`MirrordCiStore::sessions_dir`].
    sessions_dir: PathBuf,
}

impl CiStopCommandHandler {
    pub(super) fn new(sessions_dir: PathBuf, progress: ProgressTracker) -> Self {
        Self {
            progress,
            sessions_dir,
        }
    }

    /// Terminates the processes recorded in each session's [`MirrordCiStore`], and removes the
    /// directory of each session whose targets are all gone.
    ///
    /// The recorded user process groups are signaled as groups, so that descendants of a wrapper
    /// command (the server an `npm start` left behind, for instance) are terminated too.
    #[cfg(not(target_os = "windows"))]
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(super) async fn handle(self) -> CiResult<()> {
        self.handle_with(runtime_remove_container).await
    }

    /// Stops the sessions one at a time, each under its own lock, so no lock is held across
    /// sessions.
    ///
    /// A session whose lock another process holds is skipped without waiting for the lock. That
    /// process is usually another `mirrord ci stop`, and that stop holds the session's lock while
    /// it stops the session. Stopping a `mirrord ci container` session can take up to 10 seconds,
    /// so a wait could hold up this stop for that long per session. When parallel CI jobs on one
    /// runner each run `mirrord ci stop` in their teardown, one stop therefore skips the sessions
    /// that the other is stopping, rather than waiting for them. The skipped sessions are named in
    /// a warning, and the stop still succeeds; running it again is up to the user.
    ///
    /// A session that fails is reported and left for a later stop, and the stop goes on with the
    /// other sessions. It then fails with [`CiError::SessionsNotStopped`], which lists the
    /// sessions it could not stop, the skipped ones included.
    #[cfg(not(target_os = "windows"))]
    async fn handle_with<Remove, Removal>(self, mut remove_container: Remove) -> CiResult<()>
    where
        Remove: FnMut(MirrordCiManagedContainer) -> Removal,
        Removal: Future<Output = CiResult<()>>,
    {
        let Self {
            mut progress,
            sessions_dir,
        } = self;

        let session_dirs = Self::session_dirs(&sessions_dir).await?;
        let total = session_dirs.len();
        let mut found_targets = false;
        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        for session_dir in session_dirs {
            match Self::stop_session(&session_dir, &mut remove_container).await {
                Ok(had_targets) => found_targets |= had_targets,
                Err(CiError::SessionLocked { .. }) => skipped.push(session_dir),
                Err(error) => {
                    progress.warning(&format!(
                        "could not stop the mirrord ci session in {}: {error}",
                        session_dir.display()
                    ));
                    failures.push((session_dir, error));
                }
            }
        }

        if failures.is_empty().not() {
            return Err(CiError::SessionsNotStopped {
                total,
                failures,
                skipped,
            });
        }

        if skipped.is_empty().not() {
            let warning = format!(
                "`mirrord ci stop` skipped these mirrord ci sessions, because another mirrord \
                 process held their lock:{}\n\
                 To stop them, run `mirrord ci stop` again once the other mirrord process is done.",
                skipped
                    .iter()
                    .map(|session_dir| format!("\n- {}", session_dir.display()))
                    .collect::<String>()
            );
            // The spinner (`line_mode` is `None`) shows a warning only on a terminal, but it prints
            // its print buffer everywhere, so a CI log shows the warning too.
            match progress.line_mode() {
                Some(_) => progress.warning(&warning),
                None => progress.add_to_print_buffer(&warning),
            }
            progress.success(None);
            return Ok(());
        }

        // If `ci stop` is issued multiple time, we should exit with success status.
        if found_targets.not() {
            progress.success(Some(
                "No mirrord ci processes found. \
                You can also manually stop mirrord by searching for the pids with \
                `ps | grep mirrord` and calling `kill [pid]`.
                ",
            ));
            return Ok(());
        }

        progress.success(None);
        Ok(())
    }

    /// Lists the session directories in `sessions_dir`, which doesn't exist before the first
    /// session starts.
    #[cfg(not(target_os = "windows"))]
    async fn session_dirs(sessions_dir: &Path) -> CiResult<Vec<PathBuf>> {
        let mut entries = match fs::read_dir(sessions_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };

        let mut session_dirs = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            match entry.file_type().await {
                Ok(file_type) if file_type.is_dir() => session_dirs.push(entry.path()),
                Ok(_) => {}
                // On a filesystem that doesn't report the type of a directory entry, `file_type`
                // has to stat the entry, and another stop can remove a session's directory
                // between the listing and that stat. Such a session has nothing left to stop.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(session_dirs)
    }

    /// Stops the session in `session_dir` if its lock is free, and returns whether it had anything
    /// to stop.
    ///
    /// Fails with [`CiError::SessionLocked`] right away, without waiting, if another process holds
    /// the lock, so that [`Self::handle_with`] can skip the session.
    ///
    /// Holds the lock from before it reads the state until it writes the state back or removes the
    /// session, so that no process of the session can write the state in between. Such a write
    /// would bring back targets that the stop already signalled, or the stop's write would drop a
    /// target that the stop never saw. The session's directory is removed under the same lock once
    /// every target is gone; otherwise only the failed targets are written back, for a later stop
    /// to retry. A session whose directory is gone by the time the stop takes its lock was
    /// removed by another stop, so it has nothing left to stop.
    #[cfg(not(target_os = "windows"))]
    async fn stop_session<Remove, Removal>(
        session_dir: &Path,
        remove_container: Remove,
    ) -> CiResult<bool>
    where
        Remove: FnMut(MirrordCiManagedContainer) -> Removal,
        Removal: Future<Output = CiResult<()>>,
    {
        let Some(lock) = MirrordCiStore::lock_existing_session(session_dir, Duration::ZERO).await?
        else {
            return Ok(false);
        };
        let mut store =
            MirrordCiStore::read_from_file_or_default(&MirrordCiStore::state_path(session_dir))
                .await?;
        let had_targets = store.is_empty().not();

        let cleanup_result = store.terminate_with(remove_container).await;
        if store.is_empty() {
            MirrordCiStore::remove_session(session_dir, &lock).await?;
        } else {
            store.write_locked(session_dir, &lock).await?;
        }
        cleanup_result?;

        Ok(had_targets)
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

    /// Timeout of the lock waits of the sessions' own processes in these tests, so that a test
    /// whose lock stays held fails after a second.
    const LOCK_TIMEOUT: Duration = Duration::from_secs(1);

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
            spawn_background_user_command(&mut command).expect("failed to spawn script");
        store
            .user_process_groups
            .insert(child.id().expect("spawned child has a pid"));
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
        let mut child = spawn_background_user_command(&mut command).expect("failed to spawn");
        let pid = child.id().expect("spawned child has a pid");
        store.user_process_groups.insert(pid);
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

    /// A stop that can't take a session's lock must skip that session right away and leave its
    /// targets alone, because a failed state update after signalling would leave stale numeric
    /// IDs on disk for a later stop to target again. The stop must still stop the other sessions
    /// and succeed, and once the lock is free, a later stop must hold the session's lock while it
    /// cleans the session up.
    #[tokio::test]
    async fn contested_session_lock_skips_only_that_session() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions_dir = temp_dir.path();
        let contested_dir = sessions_dir.join("contested");
        let free_dir = sessions_dir.join("free");

        let mut contested = MirrordCiStore::default();
        let (mut contested_child, _contested_stdout) =
            spawn_ready_script("echo ready; while :; do :; done", &mut contested).await;
        contested
            .sidecar_containers
            .insert(MirrordCiManagedContainer {
                runtime: ContainerRuntime::Podman,
                container_id: "test-container".to_owned(),
            });
        let mut free = MirrordCiStore::default();
        let (mut free_child, _free_stdout) =
            spawn_ready_script("echo ready; while :; do :; done", &mut free).await;
        for (session_dir, store) in [(&contested_dir, &contested), (&free_dir, &free)] {
            MirrordCiStore::update(session_dir, LOCK_TIMEOUT, |persisted| {
                *persisted = store.clone()
            })
            .await
            .unwrap();
        }

        let lock = MirrordCiStore::lock_or_create_session(&contested_dir, Duration::ZERO)
            .await
            .unwrap();
        timeout(
            Duration::from_secs(1),
            CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
                .handle_with(runtime_remove_container),
        )
        .await
        .expect("the stop skips the contested session without waiting for its lock")
        .unwrap();
        timeout(Duration::from_secs(5), free_child.wait())
            .await
            .expect("the uncontested session is stopped")
            .unwrap();
        assert!(free_dir.exists().not());
        assert!(contested_child.try_wait().unwrap().is_none());
        let persisted =
            MirrordCiStore::read_from_file_or_default(&MirrordCiStore::state_path(&contested_dir))
                .await
                .unwrap();
        assert_eq!(persisted.user_process_groups, contested.user_process_groups);
        assert_eq!(persisted.sidecar_containers, contested.sidecar_containers);
        drop(lock);

        let stop = CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null());
        stop.handle_with(|_| {
            let contested_dir = contested_dir.clone();
            async move {
                assert!(matches!(
                    MirrordCiStore::update(&contested_dir, Duration::from_millis(100), |_| {})
                        .await,
                    Err(CiError::SessionLocked { .. })
                ));
                Ok(())
            }
        })
        .await
        .unwrap();
        timeout(Duration::from_secs(5), contested_child.wait())
            .await
            .expect("group exits after successful stop")
            .unwrap();
        assert!(contested_dir.exists().not());
    }

    /// Two stops that run concurrently (parallel CI jobs on one runner, each with `mirrord ci stop`
    /// in its teardown) both go over every session. When one stop removes a session after the other
    /// stop listed it, the other stop must find nothing left to stop there rather than fail, and
    /// leave no session directory behind.
    #[tokio::test]
    async fn session_removed_by_another_stop_has_nothing_to_stop() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions_dir = temp_dir.path();
        for name in ["session-1", "session-2", "session-3"] {
            MirrordCiStore::update(&sessions_dir.join(name), LOCK_TIMEOUT, |store| {
                store.sidecar_containers.insert(MirrordCiManagedContainer {
                    runtime: ContainerRuntime::Podman,
                    container_id: "test-container".to_owned(),
                });
            })
            .await
            .unwrap();
        }

        // The other stop holds the lock of the session that the directory listing puts last, and
        // removes that session while this stop removes the container of the first session. This
        // stop lists the unchanged directory in the same order, so it reaches the removed session
        // only afterwards, whatever order the filesystem lists the names in.
        let session_dirs = std::fs::read_dir(sessions_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let [stopped_dirs @ .., removed_dir] = session_dirs.as_slice() else {
            panic!("the test created three sessions, found {session_dirs:?}");
        };
        let mut other_stop_lock = Some(
            MirrordCiStore::lock_or_create_session(removed_dir, Duration::ZERO)
                .await
                .unwrap(),
        );
        let mut removals = 0;
        CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
            .handle_with(|_| {
                removals += 1;
                let other_stop_lock = other_stop_lock.take();
                let removed_dir = removed_dir.clone();
                async move {
                    if let Some(lock) = other_stop_lock {
                        MirrordCiStore::remove_session(&removed_dir, &lock)
                            .await
                            .unwrap();
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();

        assert_eq!(removals, stopped_dirs.len());
        assert!(std::fs::read_dir(sessions_dir).unwrap().next().is_none());
    }

    /// A writer that is killed after it created its temporary state file, and before it replaced
    /// the state file with it, leaves that file in the session's directory. The stop must still
    /// remove the directory, or every later stop would go over the session again.
    #[tokio::test]
    async fn stop_removes_temporary_state_file_of_killed_writer() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions_dir = temp_dir.path();
        let session_dir = sessions_dir.join("session");
        MirrordCiStore::update(&session_dir, LOCK_TIMEOUT, |_| {})
            .await
            .unwrap();
        tempfile::Builder::new()
            .prefix(MirrordCiStore::TEMPORARY_STATE_FILE_PREFIX)
            .tempfile_in(&session_dir)
            .unwrap()
            .keep()
            .unwrap();

        CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
            .handle_with(runtime_remove_container)
            .await
            .unwrap();
        assert!(session_dir.exists().not());
    }

    /// A failed container removal must not leave an already-signalled group in the retry ledger.
    #[tokio::test]
    async fn partial_failure_retries_only_failed_container() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions_dir = temp_dir.path();
        let session_dir = sessions_dir.join("session");
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) =
            spawn_ready_script("echo ready; while :; do :; done", &mut store).await;
        let container = MirrordCiManagedContainer {
            runtime: ContainerRuntime::Podman,
            container_id: "test-container".to_owned(),
        };
        store.sidecar_containers.insert(container.clone());
        MirrordCiStore::update(&session_dir, LOCK_TIMEOUT, |persisted| {
            *persisted = store.clone()
        })
        .await
        .unwrap();

        let error = CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
            .handle_with(|_| async {
                Err(CiError::IO(std::io::Error::other("runtime unavailable")))
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CiError::SessionsNotStopped { failures, .. }
                if matches!(failures.as_slice(), [(_, CiError::IO(_))])
        ));
        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("the user group was signalled")
            .unwrap();

        let retained =
            MirrordCiStore::read_from_file_or_default(&MirrordCiStore::state_path(&session_dir))
                .await
                .unwrap();
        assert!(retained.user_process_groups.is_empty());
        assert_eq!(retained.sidecar_containers, HashSet::from([container]));
        CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
            .handle_with(|_| async { Ok(()) })
            .await
            .unwrap();
        assert!(session_dir.exists().not());
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

    /// CI cleanup commonly runs `mirrord ci stop` unconditionally, so the stop must be idempotent:
    /// the first stop removes the session's directory, and a second stop finds no session and
    /// still succeeds.
    #[tokio::test]
    async fn ci_stop_removes_persisted_state_and_second_stop_succeeds() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions_dir = temp_dir.path();
        let session_dir = sessions_dir.join("session");
        let mut store = MirrordCiStore::default();
        let (mut child, _stdout) = spawn_ready_script(
            "trap 'exit 0' TERM; echo ready; while :; do :; done",
            &mut store,
        )
        .await;
        assert!(store.is_empty().not());
        tokio::fs::create_dir_all(&session_dir).await.unwrap();
        tokio::fs::write(
            MirrordCiStore::state_path(&session_dir),
            serde_json::to_vec(&store).unwrap(),
        )
        .await
        .unwrap();

        CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
            .handle()
            .await
            .unwrap();

        timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("persisted target exits")
            .unwrap();
        assert!(session_dir.exists().not());

        CiStopCommandHandler::new(sessions_dir.to_owned(), ProgressTracker::null())
            .handle()
            .await
            .unwrap();
        assert!(session_dir.exists().not());
    }
}
