use std::{error::Error, sync::Arc};

use futures::{
    FutureExt, TryFutureExt,
    future::{BoxFuture, Shared},
};
use tokio::task::JoinHandle;
use tracing::Level;

use crate::error::AgentError;

/// Converts a [`JoinHandle`] created from [`super::BgTaskRuntime::spawn`] into a [`BgTaskStatus`]
/// so we can wait and see if the task has ended properly (or not).
pub(crate) trait IntoStatus {
    /// [`tokio::spawn`]s a task that we use to get the result out of a `BackgroundTask`, so we can
    /// check it with [`BgTaskStatus::wait`] or [`BgTaskStatus::wait_assert_running`].
    fn into_status(self, task_name: &'static str) -> BgTaskStatus;
}

/// After spawning a `BackgroundTask` in a separate [`super::BgTaskRuntime`], we call
/// [`IntoStatus::into_status`] so we can [`BgTaskStatus::wait`] for the result of the task and exit
/// the agent cleanly.
#[derive(Clone)]
pub(crate) struct BgTaskStatus {
    /// Name of the `BackgroundTask`.
    ///
    /// Useful to know, when things go wrong.
    task_name: &'static str,

    /// Call `await` on this to get the result of the `BackgroundTask`.
    result: Shared<BoxFuture<'static, Result<(), Arc<dyn Error + Send + Sync>>>>,
}

impl BgTaskStatus {
    /// Waits for the future to finish and returns its result.
    ///
    /// Should the future fail or panic, this function will return
    /// [`AgentError::BackgroundTaskFailed`].
    #[tracing::instrument(level = Level::DEBUG, err)]
    pub(crate) async fn wait(&self) -> Result<(), AgentError> {
        match self.result.clone().await {
            Ok(()) => Ok(()),
            Err(error) => Err(AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error,
            }),
        }
    }

    /// Waits for the future to finish and returns its result.
    ///
    /// This function always returns [`AgentError::BackgroundTaskFailed`]. Use it when the task is
    /// not expected to finish yet, e.g. when we send a message to the `BackgroundTask` through its
    /// channel, and `send` returns an error.
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub async fn wait_assert_running(&self) -> AgentError {
        match self.result.clone().await {
            Ok(()) => AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error: Box::<dyn Error + Send + Sync>::from("task finished unexpectedly").into(),
            },
            Err(error) => AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error,
            },
        }
    }
}

impl core::fmt::Debug for BgTaskStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BgTaskStatus")
            .field("task_name", &self.task_name)
            .field("result", &self.result.clone().now_or_never())
            .finish()
    }
}

impl IntoStatus for JoinHandle<()> {
    #[tracing::instrument(level = Level::DEBUG, ret)]
    fn into_status(self, task_name: &'static str) -> BgTaskStatus {
        BgTaskStatus {
            task_name,
            result: self
                .map_err(|join_error| Arc::new(join_error) as Arc<dyn Error + Send + Sync>)
                .boxed()
                .shared(),
        }
    }
}

impl<E> IntoStatus for JoinHandle<Result<(), E>>
where
    E: Error + Send + Sync + 'static,
{
    #[tracing::instrument(level = Level::DEBUG, ret)]
    fn into_status(self, task_name: &'static str) -> BgTaskStatus {
        BgTaskStatus {
            task_name,
            result: self
                .map_ok_or_else(
                    |join_error| Err(Arc::new(join_error) as Arc<dyn Error + Send + Sync>),
                    |result| {
                        result.map_err(|error| Arc::new(error) as Arc<dyn Error + Send + Sync>)
                    },
                )
                .boxed()
                .shared(),
        }
    }
}
