use drain::Watch;
use mirrord_progress::{Progress, ProgressTracker};
use tracing::Level;

use super::{CiResult, MirrordCi};
use crate::{CliResult, ExecArgs, ci::MIRRORD_FOR_CI_INTPROXY_PID, exec, user_data::UserData};

pub(super) struct CiStartCommandHandler<'a> {
    pub(crate) mirrord_for_ci: MirrordCi,
    pub(crate) exec_args: Box<ExecArgs>,
    pub(crate) watch: Watch,
    pub(crate) user_data: &'a mut UserData,
    pub(crate) progress: ProgressTracker,
}

impl<'a> CiStartCommandHandler<'a> {
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(
        exec_args: Box<ExecArgs>,
        watch: Watch,
        user_data: &'a mut UserData,
    ) -> CiResult<Self> {
        let mut progress = ProgressTracker::from_env("mirrord ci start");

        let mirrord_for_ci = MirrordCi::get().await?;

        if mirrord_for_ci.intproxy_pid.is_some() {
            progress.failure(Some(&format!(
                "Env var {MIRRORD_FOR_CI_INTPROXY_PID} is already set,\
                 this means that `mirrord ci start` is already running!\
                  Currently `mirrord ci start` only supports 1 execution."
            )));
        }

        Ok(Self {
            mirrord_for_ci,
            exec_args,
            watch,
            user_data,
            progress,
        })
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(super) async fn handle(self) -> CliResult<()> {
        let Self {
            mirrord_for_ci,
            exec_args,
            watch,
            user_data,
            mut progress,
        } = self;

        exec(
            &exec_args,
            watch,
            user_data,
            &mut progress,
            Some(mirrord_for_ci),
        )
        .await
    }
}
