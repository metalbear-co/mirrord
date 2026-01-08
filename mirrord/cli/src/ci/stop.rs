use mirrord_progress::{Progress, ProgressTracker};
use tracing::Level;

use super::CiResult;
use crate::ci::MirrordCiStore;
#[cfg(unix)]
use crate::ci::error::CiError;

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

        let intproxies_killed = store
            .intproxy_pids
            .into_iter()
            .map(try_kill)
            .collect::<CiResult<()>>();

        let users_killed = store
            .user_pids
            .into_iter()
            .filter_map(|user_pid| Some(try_kill(user_pid?)))
            .collect::<CiResult<()>>();

        MirrordCiStore::remove_file().await?;

        progress.success(None);
        intproxies_killed.and(users_killed)
    }

    #[cfg_attr(windows, allow(unused))]
    #[cfg(target_os = "windows")]
    pub(super) async fn handle(self) -> CiResult<()> {
        unimplemented!("Command not supported on windows.");
    }
}
