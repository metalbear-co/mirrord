use drain::Watch;
use mirrord_progress::ProgressTracker;

use crate::{
    CliResult, ExecParams, MirrordCi, ci::*, config::RuntimeArgs, container::container_command,
    user_data::UserData,
};

/// Handles the `mirrord ci container` command.
///
/// Behaves in a similar way to `mirrord container`, except that we `spawn` the user's binary with
/// mirrord, letting them live after the mirrord cli has died.
///
/// The fields in here are passed to [`container_command`], since we re-use that to start mirrord in
/// CI.
pub(super) struct CiContainerCommandHandler<'a> {
    /// Used to tell [`container_command`] that we're running in ci.
    mirrord_for_ci: MirrordCi,

    /// Container runtime (e.g. docker).
    runtime_args: RuntimeArgs,

    /// [`container_command`] uses these [`ExecParams`] to run.
    exec_params: ExecParams,

    /// Used by [`mirrord_analytics::AnalyticsReporter`].
    watch: Watch,

    /// See [`UserData`]. `mirrord ci container` doesn't do anything special with it, just passes
    /// it to [`container_command`].
    user_data: &'a mut UserData,

    /// Initialized with `mirrord ci container` instead of `mirrord container`.
    progress: ProgressTracker,
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
        let progress = ProgressTracker::from_env("mirrord ci container");

        let mirrord_for_ci = MirrordCi::new(container_args.ci_common_args).await?;

        let (runtime_args, exec_params) = container_args.container_args.into_parts();

        Ok(Self {
            mirrord_for_ci,
            runtime_args,
            exec_params,
            watch,
            user_data,
            progress,
        })
    }

    /// Calls [`container_command`] with [`MirrordCi`]
    pub(super) async fn handle(self) -> CliResult<()> {
        let Self {
            mirrord_for_ci,
            runtime_args,
            exec_params,
            watch,
            user_data,
            mut progress,
        } = self;

        container_command(
            runtime_args,
            exec_params,
            watch,
            user_data,
            &mut progress,
            Some(mirrord_for_ci),
        )
        .await
        .map(|_| ())
    }
}
