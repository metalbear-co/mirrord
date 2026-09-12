#![cfg_attr(windows, allow(unused))]
use std::time::{Duration, Instant};

use itertools::Itertools;
use mirrord_progress::{Progress, ProgressTracker};
#[cfg(not(target_os = "windows"))]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tokio::process::Command;
use tracing::Level;

use super::CiResult;
#[cfg(not(target_os = "windows"))]
use crate::ci::CiError;
use crate::ci::MirrordCiStore;

/// How long the user's process gets to shut down on `SIGTERM` before it is `SIGKILL`ed.
const USER_PROCESS_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// How often we check whether the user's process is gone during [`USER_PROCESS_GRACE_PERIOD`].
const USER_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Kills the sidecars that were started by `mirrord ci container`.
///
/// When running `mirrord ci container`, the `intproxy` is started as `root`, so a regular user
/// won't be able to kill it with `mirrord ci stop`, and thus we need to use something
/// like `docker rm` to stop it.
#[cfg(not(target_os = "windows"))]
async fn runtime_remove_container(container: crate::ci::MirrordCiManagedContainer) -> CiResult<()> {
    use crate::ci::error::CiError;

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

/// Sends `signal` to `target`, reporting whether `target` was still around to receive it.
///
/// A [`None`] signal only probes for existence. A negative `target` is a process group.
#[cfg(not(target_os = "windows"))]
fn try_signal(target: Pid, signal: impl Into<Option<Signal>>) -> CiResult<bool> {
    match kill(target, signal) {
        Ok(()) => Ok(true),
        // ESRCH means that the process has already exited.
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(CiError::from(error)),
    }
}

/// `SIGKILL`s `pid`, used for the processes mirrord itself started.
#[cfg(not(target_os = "windows"))]
fn try_kill(pid: u32) -> CiResult<()> {
    try_signal(Pid::from_raw(pid as i32), Signal::SIGKILL)?;

    Ok(())
}

/// Terminates the process group led by `pid`, falling back to the process on its own.
///
/// `mirrord ci start` makes the user's command a process group leader, so its pid doubles as the
/// group id. Signalling the group is what reaches the descendants of wrappers such as `npm run`,
/// which otherwise survive - still holding the listening port - the death of the wrapper.
///
/// `SIGTERM` goes first so that servers get to close their listening sockets, and whatever is
/// still alive after [`USER_PROCESS_GRACE_PERIOD`] gets `SIGKILL`ed.
#[cfg(not(target_os = "windows"))]
async fn try_kill_user_process(pid: u32) -> CiResult<()> {
    let process = Pid::from_raw(pid as i32);
    let group = Pid::from_raw(-(pid as i32));

    // Commands recorded by `mirrord ci container`, or by an older mirrord, lead no group of their
    // own, so there is nothing to signal but the process itself.
    let target = if try_signal(group, None)? {
        group
    } else {
        process
    };

    if !try_signal(target, Signal::SIGTERM)? {
        return Ok(());
    }

    let deadline = Instant::now() + USER_PROCESS_GRACE_PERIOD;
    while Instant::now() < deadline {
        tokio::time::sleep(USER_PROCESS_POLL_INTERVAL).await;

        if !try_signal(target, None)? {
            return Ok(());
        }
    }

    try_signal(target, Signal::SIGKILL)?;

    Ok(())
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
}

impl CiStopCommandHandler {
    /// Builds the [`MirrordCiStore`], checking if the mirrord-for-ci requirements have been met.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new() -> CiResult<Self> {
        let progress = ProgressTracker::from_env("mirrord ci stop");

        let store = MirrordCiStore::read_from_file_or_default().await?;

        Ok(Self { store, progress })
    }

    /// [`kill`](nix::sys::signal::kill)s the intproxy and the user's process, using the pids stored
    /// in [`MirrordCiStore`].
    #[cfg(not(target_os = "windows"))]
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(super) async fn handle(self) -> CiResult<()> {
        use futures::{StreamExt, future, stream};

        let Self {
            store,
            mut progress,
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

        // The user's processes go first, while the proxies are still up, so that whatever cleanup
        // they do on `SIGTERM` can still reach the cluster. Killed concurrently, so that their
        // grace periods overlap.
        let users_killed = future::join_all(
            store
                .user_pids
                .into_iter()
                .flatten()
                .map(try_kill_user_process),
        )
        .await;

        // We don't want to short-circuit on error, go to the next pid and try to `kill` it.
        let intproxies_killed = store
            .intproxy_pids
            .into_iter()
            .map(try_kill)
            .collect::<Vec<_>>();

        let sidecars_removed = stream::iter(store.sidecar_containers)
            .then(runtime_remove_container)
            .collect::<Vec<_>>()
            .await;

        let extproxies_killed = store
            .extproxy_pids
            .into_iter()
            .map(try_kill)
            .collect::<Vec<_>>();

        let sidecars_killed = store
            .sidecar_pids
            .into_iter()
            .map(try_kill)
            .collect::<Vec<_>>();

        users_killed
            .into_iter()
            .try_collect::<_, (), _>()
            .and(intproxies_killed.into_iter().try_collect::<_, (), _>())
            .and(sidecars_removed.into_iter().try_collect::<_, (), _>())
            .and(extproxies_killed.into_iter().try_collect::<_, (), _>())
            .and(sidecars_killed.into_iter().try_collect::<_, (), _>())?;

        MirrordCiStore::remove_file().await?;
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
mod test {
    use std::process::Stdio;

    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    /// Waits for `pid` to disappear, returning `false` if it is still around after a few seconds.
    async fn wait_for_exit(pid: Pid) -> bool {
        for _ in 0..100 {
            if !try_signal(pid, None).unwrap() {
                return true;
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        false
    }

    /// `mirrord ci stop` has to reach the descendants of the command it started, not just the
    /// command itself.
    ///
    /// A wrapper that runs the real server as a child of its own - `npm run dev` and friends - used
    /// to leave that child alive and holding its listening port.
    #[tokio::test]
    async fn kills_descendants_of_the_user_command() {
        // Stands in for the wrapper: spawns a grandchild, reports its pid, and waits for it.
        let mut wrapper = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 100 & echo $! ; wait")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("failed to spawn the wrapper");

        let mut stdout = BufReader::new(wrapper.stdout.take().expect("stdout was piped")).lines();
        let grandchild = stdout
            .next_line()
            .await
            .expect("failed to read the grandchild pid")
            .expect("wrapper printed no grandchild pid")
            .trim()
            .parse::<i32>()
            .expect("wrapper printed a malformed grandchild pid");
        let grandchild = Pid::from_raw(grandchild);

        let wrapper_pid = wrapper.id().expect("wrapper should still be running");
        // Reaping the wrapper in parallel, as the caller of `mirrord ci start` would.
        let (killed, _) = tokio::join!(try_kill_user_process(wrapper_pid), wrapper.wait());
        killed.expect("failed to kill the user process");

        assert!(
            wait_for_exit(grandchild).await,
            "grandchild {grandchild} survived `mirrord ci stop`"
        );
    }
}
