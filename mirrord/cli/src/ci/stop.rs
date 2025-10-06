use std::process::ExitStatus;

use tokio::process::Command;

use super::CiResult;
use crate::{MirrordCi, ci::error::CiError};

pub(super) struct CiStopCommandHandler {
    pub(crate) mirrord_ci: MirrordCi,
}

impl CiStopCommandHandler {
    pub(super) async fn new() -> CiResult<Self> {
        let mirrord_ci = MirrordCi::get().await?;

        Ok(Self { mirrord_ci })
    }

    pub(super) async fn handle(self) -> CiResult<ExitStatus> {
        let Self { mirrord_ci } = self;

        if let Some(intproxy_pid) = mirrord_ci.intproxy_pid {
            let kill = Command::new("kill")
                .arg(intproxy_pid.to_string())
                .spawn()?
                .wait()
                .await?;

            mirrord_ci.clear().await?;

            Ok(kill)
        } else {
            Err(CiError::IntproxyPidMissing)
        }
    }
}
