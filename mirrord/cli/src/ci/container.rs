use std::path::PathBuf;

use drain::Watch;
use mirrord_progress::ProgressTracker;
use tracing::Level;

use crate::{
    CliResult, ExecParams, MirrordCi,
    ci::*,
    config::{ExtensionContainerArgs, RuntimeArgs},
    container::{container_command, container_ext_command},
    user_data::UserData,
};

pub(super) struct CiContainerCommandHandler<'a> {
    mirrord_for_ci: MirrordCi,

    runtime_args: RuntimeArgs,
    exec_params: ExecParams,

    /// Used by [`mirrord_analytics::AnalyticsReporter`].
    watch: Watch,

    user_data: &'a mut UserData,

    progress: ProgressTracker,
}

pub(super) struct CiExtensionContainerCommandHandler<'a> {
    mirrord_for_ci: MirrordCi,

    config_file: Option<PathBuf>,
    target: Option<String>,

    /// Used by [`mirrord_analytics::AnalyticsReporter`].
    watch: Watch,

    user_data: &'a mut UserData,

    progress: ProgressTracker,
}

impl<'a> CiContainerCommandHandler<'a> {
    /// Starts a [`ProgressTracker`] and performs some checks to see if the mirrord for ci
    /// requirements have been met.
    // #[tracing::instrument(level = Level::TRACE, err)]
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

    pub(super) async fn handle(self) -> CliResult<i32> {
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
    }
}

impl<'a> CiExtensionContainerCommandHandler<'a> {
    /// Starts a [`ProgressTracker`] and performs some checks to see if the mirrord for ci
    /// requirements have been met.
    #[tracing::instrument(level = Level::DEBUG, err)]
    pub(super) async fn new(
        container_args: CiExtensionContainerArgs,
        watch: Watch,
        user_data: &'a mut UserData,
    ) -> CiResult<Self> {
        let progress = ProgressTracker::from_env("mirrord ci extension container");

        let CiExtensionContainerArgs {
            extension_container_args:
                ExtensionContainerArgs {
                    config_file,
                    target,
                },
            ci_common_args,
        } = container_args;

        let mirrord_for_ci = MirrordCi::new(ci_common_args).await?;

        Ok(Self {
            mirrord_for_ci,
            config_file,
            target,
            watch,
            user_data,
            progress,
        })
    }

    pub(super) async fn handle(self) -> CliResult<()> {
        let Self {
            mirrord_for_ci,
            config_file,
            target,
            watch,
            user_data,
            mut progress,
        } = self;

        container_ext_command(
            config_file,
            target,
            watch,
            user_data,
            &mut progress,
            Some(mirrord_for_ci),
        )
        .await
    }
}
