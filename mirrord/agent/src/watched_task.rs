use std::{
    future::Future,
    sync::{Arc, RwLock, RwLockReadGuard},
};

use crate::error::AgentError;

/// A shared clonable view on a background task's status.
#[derive(Debug, Clone)]
pub struct TaskStatus {
    /// Name of the task.
    task_name: &'static str,
    /// An error that occurred during the execution of the task.
    error: Arc<RwLock<Option<AgentError>>>,
}

impl TaskStatus {
    /// Check if an error occurred during the execution of the task.
    /// If yes, return [`AgentError::BackgroundTaskFailed`].
    pub fn check(&self) -> Option<AgentError> {
        self.error
            .read()
            .expect("task status lock poisoned")
            .as_ref()
            .map(|e| AgentError::BackgroundTaskFailed {
                task: self.task_name,
                cause: e.to_string(),
            })
    }

    /// Return a view over the error that occurred during the execution of the task.
    /// This guard should not be held for a long time, especially across .await points.
    pub fn err(&self) -> RwLockReadGuard<'_, Option<AgentError>> {
        self.error.read().expect("task status lock poisoned")
    }
}

/// A wrapper around asynchronous task.
/// Captures the task's status and exposes it through [`TaskStatus`].
pub(crate) struct WatchedTask<F> {
    /// Shared view on the task status.
    status: TaskStatus,
    /// The task to be executed.
    task: F,
}

impl<F> WatchedTask<F> {
    pub(crate) fn new(task_name: &'static str, task: F) -> Self {
        Self {
            status: TaskStatus {
                task_name,
                error: Default::default(),
            },
            task,
        }
    }

    /// Return a shared view over the inner [`TaskStatus`].
    pub(crate) fn status(&self) -> TaskStatus {
        self.status.clone()
    }
}

impl<T> WatchedTask<T>
where
    T: Future<Output = Result<(), AgentError>>,
{
    /// Execute the wrapped task.
    /// If an error occurred during the execution, store it in the inner [`TaskStatus`].
    pub(crate) async fn start(self) {
        if let Err(e) = self.task.await {
            self.status
                .error
                .write()
                .expect("task status lock poisoned")
                .replace(e);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn simple_successful() {
        let task = WatchedTask::new("task", async move { Ok(()) });
        let status = task.status();
        task.start().await;
        assert!(status.check().is_none());
        assert!(status.err().is_none());
    }

    #[tokio::test]
    async fn simple_failing() {
        let task = WatchedTask::new("task", async move { Err(AgentError::ReceiverClosed) });
        let status = task.status();
        task.start().await;
        assert!(status.check().is_some());
        assert!(status.err().is_some());
    }
}
