use tracing::Level;

use super::CiResult;
use crate::MirrordCi;
#[cfg(not(target_os = "windows"))]
use crate::ci::error::CiError;

/// Handles the `mirrord ci stop` command.
///
/// Builds a [`MirrordCi`] to kill the intproxy and the user's binary that was started by `mirrord
/// ci start`.
pub(super) struct CiStopCommandHandler {
    /// The [`MirrordCi`] we retrieve from the user's environment (env var and temp files) so we
    /// can kill the intproxy and the user's process.
    #[cfg_attr(target_os = "windows", allow(unused))]
    pub(crate) mirrord_ci: MirrordCi,
}

impl CiStopCommandHandler {
    /// Builds the [`MirrordCi`], checking if the mirrord-for-ci requirements have been met.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new() -> CiResult<Self> {
        let mirrord_ci = MirrordCi::get().await?;

        Ok(Self { mirrord_ci })
    }

    /// [`kill`](nix::sys::signal::kill)s the intproxy and the user's process, using the pids stored
    /// in [`MirrordCi`].
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

        let Self { mirrord_ci } = self;

        let intproxy_killed = match mirrord_ci.store.intproxy_pid {
            Some(pid) => try_kill(pid),
            None => Err(CiError::IntproxyPidMissing),
        };

        let user_killed = match mirrord_ci.store.user_pid {
            Some(pid) => try_kill(pid),
            None => Err(CiError::UserPidMissing),
        };

        mirrord_ci.clear().await?;

        intproxy_killed.and(user_killed)
    }

    #[cfg(target_os = "windows")]
    pub(super) async fn handle(self) -> CiResult<()> {
        unimplemented!("Command not supported on windows.");
    }
}
