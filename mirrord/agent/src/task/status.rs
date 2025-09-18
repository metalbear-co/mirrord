use std::{error::Error, sync::Arc};

use futures::{FutureExt, future::Shared};
use tokio::{sync::oneshot, task::JoinHandle};
use tracing::Level;

use crate::{error::AgentError, util::error::BgTaskPanicked};

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
    result: Shared<oneshot::Receiver<Result<(), Arc<dyn Error + Send + Sync>>>>,
}

impl BgTaskStatus {
    /// Waits for the future to finish and returns its result.
    ///
    /// Should the future fail or panic, this function will return
    /// [`AgentError::BackgroundTaskFailed`].
    #[tracing::instrument(level = Level::DEBUG, fields(rt, rt_metrics), err)]
    pub(crate) async fn wait(&self) -> Result<(), AgentError> {
        let handle = tokio::runtime::Handle::current();
        tracing::Span::current().record("rt", format!("{handle:?}"));
        tracing::Span::current().record("rt_metrics", format!("{:?}", handle.metrics()));

        match self.result.clone().await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error,
            }),
            Err(..) => Err(AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error: Arc::new(BgTaskPanicked) as Arc<dyn Error + Send + Sync>,
            }),
        }
    }

    /// Waits for the future to finish and returns its result.
    ///
    /// This function always returns [`AgentError::BackgroundTaskFailed`]. Use it when the task is
    /// not expected to finish yet, e.g. when we send a message to the `BackgroundTask` through its
    /// channel, and `send` returns an error.
    #[tracing::instrument(level = Level::DEBUG, fields(rt, rt_metrics), ret)]
    pub(crate) async fn wait_assert_running(&self) -> AgentError {
        let handle = tokio::runtime::Handle::current();
        tracing::Span::current().record("rt", format!("{handle:?}"));
        tracing::Span::current().record("rt_metrics", format!("{:?}", handle.metrics()));

        match self.result.clone().await {
            Ok(Ok(())) => AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error: Box::<dyn Error + Send + Sync>::from("task finished unexpectedly").into(),
            },
            Ok(Err(error)) => AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error,
            },
            Err(..) => AgentError::BackgroundTaskFailed {
                task: self.task_name,
                error: Arc::new(BgTaskPanicked) as Arc<dyn Error + Send + Sync>,
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
        let (result_tx, result_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = match self.await {
                Ok(()) => Ok(()),
                Err(..) => Err(Arc::new(BgTaskPanicked) as Arc<dyn Error + Send + Sync>),
            };

            let _ = result_tx.send(result);
        });

        BgTaskStatus {
            task_name,
            result: result_rx.shared(),
        }
    }
}

impl<E> IntoStatus for JoinHandle<Result<(), E>>
where
    E: Error + Send + Sync + 'static,
{
    #[tracing::instrument(level = Level::DEBUG, ret)]
    fn into_status(self, task_name: &'static str) -> BgTaskStatus {
        // let _guard = runtime.enter();
        let (result_tx, result_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = match self.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(Arc::new(e) as Arc<dyn Error + Send + Sync>),
                Err(..) => Err(Arc::new(BgTaskPanicked) as Arc<dyn Error + Send + Sync>),
            };

            let _ = result_tx.send(result);
        });

        BgTaskStatus {
            task_name,
            result: result_rx.shared(),
        }
    }
}
