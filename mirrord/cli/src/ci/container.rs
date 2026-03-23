use drain::Watch;
use mirrord_progress::ProgressTracker;
use tracing::Level;

use crate::{
    CiContainerArgs, CiStartArgs, CliResult, ContainerArgs, ExecArgs, MirrordCi, ci::CiResult,
    user_data::UserData,
};

pub(super) struct CiContainerCommandHandler<'a> {
    pub(crate) mirrord_for_ci: MirrordCi,

    pub(crate) exec_args: Box<ExecArgs>,

    /// Used by [`mirrord_analytics::AnalyticsReporter`].
    pub(crate) watch: Watch,

    pub(crate) user_data: &'a mut UserData,

    pub(crate) progress: ProgressTracker,
}

impl<'a> CiContainerCommandHandler<'a> {
    /// Starts a [`ProgressTracker`] and performs some checks to see if the mirrord for ci
    /// requirements have been met.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(
        container_args: Box<CiContainerArgs>,
        watch: Watch,
        user_data: &'a mut UserData,
    ) -> CiResult<Self> {
        let progress = ProgressTracker::from_env("mirrord ci start");

        let mirrord_for_ci = MirrordCi::new(&container_args.ci_args).await?;

        Ok(Self {
            mirrord_for_ci,
            exec_args: container_args.ci_args.exec_args,
            watch,
            user_data,
            progress,
        })
    }

    pub(super) async fn handle(self) -> CliResult<()> {
        todo!()
    }
}
