use drain::Watch;
use mirrord_progress::{Progress, ProgressTracker};
use tracing::Level;

use super::{CiResult, MirrordCi};
use crate::{CliResult, ExecArgs, ci::error::CiError, exec, user_data::UserData};

/// Handles the `mirrord ci start` command.
///
/// Behaves in a similar way to `mirrord exec`, except that we `spawn` the user's binary with
/// mirrord, letting them live after the mirrord cli has died.
///
/// The fields in here are passed to [`exec`], since we re-use that to start mirrord in ci.
pub(super) struct CiStartCommandHandler<'a> {
    /// Used to tell [`exec`] that we're running in ci.
    pub(crate) mirrord_for_ci: MirrordCi,

    /// Args we're passing to [`exec`], such as mirrord config file, etc.
    pub(crate) exec_args: Box<ExecArgs>,

    /// Used by [`mirrord_analytics::AnalyticsReporter`].
    pub(crate) watch: Watch,

    /// See [`UserData`]. `mirrord ci start` doesn't do anything special with it, just passes it to
    /// [`exec`].
    pub(crate) user_data: &'a mut UserData,

    /// Initialized with `mirrord ci start` instead of `mirrord exec`.
    pub(crate) progress: ProgressTracker,
}

impl<'a> CiStartCommandHandler<'a> {
    /// Starts a [`ProgressTracker`] and performs some checks to see if the mirrord for ci
    /// requirements have been met.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(
        exec_args: Box<ExecArgs>,
        watch: Watch,
        user_data: &'a mut UserData,
    ) -> CiResult<Self> {
        let mut progress = ProgressTracker::from_env("mirrord ci start");

        let mirrord_for_ci = MirrordCi::get().await?;

        if mirrord_for_ci.intproxy_pid.is_some() {
            progress.failure(Some("Detected existing intproxy pid file!"));
            Err(CiError::IntproxyPidAlreadyPresent)
        } else if mirrord_for_ci.user_pid.is_some() {
            progress.failure(Some("Detected existing user pid file!"));
            Err(CiError::UserPidAlreadyPresent)
        } else {
            Ok(Self {
                mirrord_for_ci,
                exec_args,
                watch,
                user_data,
                progress,
            })
        }
    }

    /// Calls [`exec`] with [`MirrordCi`].
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
