use tracing::Level;

use super::CiResult;
use crate::ci::MirrordCiStore;
#[cfg(not(target_os = "windows"))]
use crate::ci::error::CiError;

/// Handles the `mirrord ci stop` command.
///
/// Builds a [`MirrordCiStore`] to kill the intproxy and the user's binary that was started by
/// `mirrord ci start`.
pub(super) struct CiStopCommandHandler {
    /// The [`MirrordCiStore`] we retrieve from the user's environment (env var and temp files) so
    /// we can kill the intproxy and the user's process.
    #[cfg_attr(target_os = "windows", allow(unused))]
    pub(crate) store: MirrordCiStore,
}

impl CiStopCommandHandler {
    /// Builds the [`MirrordCiStore`], checking if the mirrord-for-ci requirements have been met.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new() -> CiResult<Self> {
        let store = MirrordCiStore::read_from_file_or_default().await?;

        Ok(Self { store })
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

        let intproxy_killed = match self.store.intproxy_pid {
            Some(pid) => try_kill(pid),
            None => Err(CiError::IntproxyPidMissing),
        };

        let user_killed = match self.store.user_pid {
            Some(pid) => try_kill(pid),
            None => Err(CiError::UserPidMissing),
        };

        MirrordCiStore::remove_file().await?;

        intproxy_killed.and(user_killed)
    }

    #[cfg(target_os = "windows")]
    pub(super) async fn handle(self) -> CiResult<()> {
        unimplemented!("Command not supported on windows.");
    }
}
