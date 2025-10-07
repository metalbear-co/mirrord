use std::process::ExitStatus;

use tokio::process::Command;
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
    pub(super) async fn handle(self) -> CiResult<ExitStatus> {
        let Self { mirrord_ci } = self;

        match mirrord_ci.intproxy_pid.zip(mirrord_ci.user_pid) {
            Some((intproxy_pid, user_pid)) => {
                let kill_intproxy = Command::new("kill")
                    .arg(intproxy_pid.to_string())
                    .spawn()?
                    .wait()
                    .await?;

                let _ = Command::new("kill")
                    .arg(user_pid.to_string())
                    .spawn()?
                    .wait()
                    .await?;

                mirrord_ci.clear().await?;

                Ok(kill_intproxy)
            }
            None => Err(CiError::IntproxyPidMissing),
        }
    }
}
