use std::process::ExitStatus;

use tokio::process::Command;
use tracing::Level;

use super::CiResult;
use crate::{MirrordCi, ci::error::CiError};

pub(super) struct CiStopCommandHandler {
    pub(crate) mirrord_ci: MirrordCi,
}

impl CiStopCommandHandler {
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new() -> CiResult<Self> {
        let mirrord_ci = MirrordCi::get().await?;

        Ok(Self { mirrord_ci })
    }

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
