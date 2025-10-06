use std::process::ExitStatus;

use tokio::process::Command;

use super::{CiResult, MIRRORD_FOR_CI_INTPROXY_PID};
use crate::{MirrordCi, ci::error::CiError};

pub(super) struct CiStopCommandHandler {
    pub(crate) mirrord_ci: MirrordCi,
}

impl CiStopCommandHandler {
    pub(super) async fn new() -> CiResult<Self> {
        let mirrord_ci = MirrordCi::get()?;

        Ok(Self { mirrord_ci })
    }

    pub(super) async fn handle(self) -> CiResult<ExitStatus> {
        let Self {
            mirrord_ci: MirrordCi { intproxy_pid, .. },
        } = self;

        if let Some(intproxy_pid) = intproxy_pid {
            let kill = Command::new("kill")
                .arg(intproxy_pid.to_string())
                .spawn()?
                .wait()
                .await?;

            unsafe {
                std::env::remove_var(MIRRORD_FOR_CI_INTPROXY_PID);
            }

            Ok(kill)
        } else {
            Err(CiError::IntproxyPidMissing)
        }
    }
}
