use nix::unistd::Pid;
use tracing::Level;

use super::CiResult;
use crate::{MirrordCi, ci::error::CiError};

/// Handles the `mirrord ci stop` command.
///
/// Builds a [`MirrordCi`] to kill the intproxy and the user's binary that was started by `mirrord
/// ci start`.
pub(super) struct CiStopCommandHandler {
    /// The [`MirrordCi`] we retrieve from the user's environment (env var and temp files) so we
    /// can kill the intproxy and the user's process.
    pub(crate) mirrord_ci: MirrordCi,
}

impl CiStopCommandHandler {
    /// Builds the [`MirrordCi`], checking if the mirrord-for-ci requirements have been met.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new() -> CiResult<Self> {
        let mirrord_ci = MirrordCi::get().await?;

        Ok(Self { mirrord_ci })
    }

    /// `kill`s the intproxy and the user's process with the pids stored in [`MirrordCi`].
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(super) async fn handle(self) -> CiResult<()> {
        use nix::sys::signal::{Signal, kill};

        let Self { mirrord_ci } = self;

        let intproxy_killed = match mirrord_ci.store.intproxy_pid {
            Some(pid) => kill(Pid::from_raw(pid as i32), Some(Signal::SIGKILL)).map_err(From::from),
            None => Err(CiError::IntproxyPidMissing),
        };

        let user_killed = match mirrord_ci.store.user_pid {
            Some(pid) => kill(Pid::from_raw(pid as i32), Some(Signal::SIGKILL)).map_err(From::from),
            None => Err(CiError::UserPidMissing),
        };

        mirrord_ci.clear().await?;

        intproxy_killed.and(user_killed)
    }
}
