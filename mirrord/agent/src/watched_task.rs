use std::future::Future;

use tokio::sync::watch::{self, Receiver, Sender};

use crate::error::AgentError;

/// A shared clonable view on a background task's status.
#[derive(Debug, Clone)]
pub struct TaskStatus {
    /// Name of the task.
    task_name: &'static str,
    /// Channel to receive the result of the task.
    /// Initially, this channel contains [`None`].
    /// Only one value should ever be sent through this channel and it should be [`Some`].
    result_rx: Receiver<Option<Result<(), AgentError>>>,
}

impl TaskStatus {
    /// Wait for the task to complete and return the error.
    /// Can be called multiple times and safely cancelled.
    ///
    /// # Panics
    /// Panic if the task has not failed.
    pub async fn unwrap_err(&mut self) -> AgentError {
        self.err().await.expect("task did not fail")
    }

    /// Wait for the task to complete.
    /// If the task has failed, return the error.
    /// Can be called multiple times and safely cancelled.
    pub async fn err(&mut self) -> Option<AgentError> {
        if self.result_rx.borrow().is_none() && self.result_rx.changed().await.is_err() {
            return Some(AgentError::BackgroundTaskFailed {
                task: self.task_name,
                cause: "task panicked".into(),
            });
        }

        self.result_rx
            .borrow()
            .as_ref()
            .expect("WatchedTask set an empty status on exit")
            .as_ref()
            .err()
            .map(|e| AgentError::BackgroundTaskFailed {
                task: self.task_name,
                cause: e.to_string(),
            })
    }
}

/// A wrapper around asynchronous task.
/// Captures the task's status and exposes it through [`TaskStatus`].
pub(crate) struct WatchedTask<F> {
    /// Shared view on the task status.
    status: TaskStatus,
    /// The task to be executed.
    task: F,
    /// Channel to send the task result.
    result_tx: Sender<Option<Result<(), AgentError>>>,
}

impl<F> WatchedTask<F> {
    pub(crate) fn new(task_name: &'static str, task: F) -> Self {
        let (result_tx, result_rx) = watch::channel(None);

        Self {
            status: TaskStatus {
                task_name,
                result_rx,
            },
            task,
            result_tx,
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
    /// Store its result in the inner [`TaskStatus`].
    pub(crate) async fn start(self) {
        let result = self.task.await;
        self.result_tx.send(Some(result)).ok(); // All receivers may be dropped.
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn simple_successful() {
        let task = WatchedTask::new("task", async move { Ok(()) });
        let mut status = task.status();
        task.start().await;
        assert!(status.err().await.is_none());
    }

    #[tokio::test]
    async fn simple_failing() {
        let task = WatchedTask::new("task", async move { Err(AgentError::TestError) });
        let mut status = task.status();
        task.start().await;
        assert!(status.err().await.is_some());
        status.unwrap_err().await;
    }
}
