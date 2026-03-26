#![cfg_attr(windows, allow(unused))]
use mirrord_progress::{Progress, ProgressTracker};
use tokio::process::Command;
use tracing::Level;

use super::CiResult;
use crate::ci::{MirrordCiManagedContainer, MirrordCiStore, error::CiError};

/// Kills the sidecars that were started by `mirrord ci container`.
///
/// When running `mirrord ci container`, the `intproxy` is started as `root`, so a regular user
/// won't be able to kill it with `mirrord ci stop`, and thus we need to use something
/// like `docker rm` to stop it.
#[cfg(not(target_os = "windows"))]
async fn runtime_remove_container(container: MirrordCiManagedContainer) -> CiResult<()> {
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
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
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
    #[cfg_attr(windows, allow(unused))]
    pub(crate) store: MirrordCiStore,

    #[cfg_attr(windows, allow(unused))]
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
        use futures::{StreamExt, stream};
        use nix::{
            errno::Errno,
            sys::signal::{Signal, kill},
            unistd::Pid,
        };

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

        fn try_kill(pid: u32) -> CiResult<()> {
            kill(Pid::from_raw(pid as i32), Some(Signal::SIGKILL))
                .or_else(|error| {
                    if error == Errno::ESRCH {
                        // ESRCH means that the process has already exited.
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(CiError::from)
        }

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

        let users_killed = store
            .user_pids
            .into_iter()
            .filter_map(|user_pid| Some(try_kill(user_pid?)))
            .collect::<Vec<_>>();

        intproxies_killed
            .into_iter()
            .try_collect::<()>()
            .and(sidecars_removed.into_iter().try_collect::<()>())
            .and(extproxies_killed.into_iter().try_collect::<()>())
            .and(sidecars_killed.into_iter().try_collect::<()>())
            .and(users_killed.into_iter().try_collect::<()>())?;

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
