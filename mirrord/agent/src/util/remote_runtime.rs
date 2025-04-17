//! Utilities for running async code in the agent target's namespace.
//!
//! Useful for running tasks that require access to the target's network namespace,
//! such as traffic stealing, traffic mirroring, DNS resolution, outgoing traffic.
//!
//! Provides:
//! 1. A [`RemoteRuntime`] struct, that can be used to run tasks in the target's namespace.
//! 2. A [`BgTaskRuntime`] enum, that don't necessarily require a target (DNS and outgoing traffic),
//!    but should be run in the target's namespace if available.
//! 3. A [`BgTaskStatus`] struct, that can be used to poll for a spawned task's status.

use std::{
    error::Error,
    fmt,
    future::Future,
    ops::Not,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    thread,
};

use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use tokio::sync::{mpsc, oneshot};

use super::error::{BgTaskPanicked, RemoteRuntimeError};
use crate::{
    error::AgentError,
    namespace::{self, NamespaceType},
};

/// A cloneable handle to a remote [`tokio::runtime::Runtime`] that runs in its own thread.
///
/// Can be used to spawn tasks with [`RemoteRuntime::spawn`].
///
/// The runtime will be aborted when all handles are dropped.
#[derive(Clone)]
pub struct RemoteRuntime {
    target_pid: u64,
    future_tx: mpsc::Sender<BoxFuture<'static, ()>>,
}

impl RemoteRuntime {
    /// Creates a new remote runtime.
    ///
    /// This runtime's thread will enter the specified namespace of the target.
    pub async fn new_in_namespace(
        target_pid: u64,
        namespace_type: NamespaceType,
    ) -> Result<Self, RemoteRuntimeError> {
        let (future_tx, mut future_rx) = mpsc::channel(16);
        let (result_tx, result_rx) = oneshot::channel();
        let thread_name = format!("remote-{target_pid}-{namespace_type}-runtime-thread");
        let thread_logic = move || {
            if let Err(error) = namespace::set_namespace(target_pid, namespace_type) {
                let _ = result_tx.send(Err(error.into()));
                return;
            }

            let rt_result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_name(format!(
                    "remote-{target_pid}-{namespace_type}-runtime-worker"
                ))
                .build();
            let rt = match rt_result {
                Ok(rt) => rt,
                Err(error) => {
                    let _ = result_tx.send(Err(RemoteRuntimeError::TokioRuntimeError(error)));
                    return;
                }
            };

            if result_tx.send(Ok(())).is_err() {
                return;
            }

            rt.block_on(async move {
                while let Some(future) = future_rx.recv().await {
                    tokio::spawn(future);
                }
            });
        };

        thread::Builder::new()
            .name(thread_name)
            .spawn(thread_logic)
            .map_err(RemoteRuntimeError::ThreadSpawnError)?;

        match result_rx.await {
            Ok(Ok(())) => Ok(Self {
                target_pid,
                future_tx,
            }),
            Ok(Err(error)) => Err(error),
            Err(..) => Err(RemoteRuntimeError::Panicked),
        }
    }

    /// Spawns the given future on this remote runtime.
    pub fn spawn<F>(&self, future: F) -> BgTask<F::Output>
    where
        F: 'static + Future + Send,
        F::Output: 'static + Send,
    {
        let (result_tx, result_rx) = oneshot::channel();

        let future = async move {
            let result = future.await;
            let _ = result_tx.send(result);
        }
        .boxed();

        let future_tx = self.future_tx.clone();
        tokio::spawn(async move {
            let _ = future_tx.send(future).await;
        });

        BgTask {
            future_result: result_rx,
        }
    }

    /// Returns the target's PID.
    pub fn target_pid(&self) -> u64 {
        self.target_pid
    }
}

impl fmt::Debug for RemoteRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteRuntime")
            .field("running", &self.future_tx.is_closed().not())
            .finish()
    }
}

/// A future spawned with [`RemoteRuntime::spawn`] or
/// [`BgTaskRuntime::spawn`]
pub struct BgTask<T> {
    future_result: oneshot::Receiver<T>,
}

impl<T> Future for BgTask<T> {
    type Output = Result<T, BgTaskPanicked>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        Pin::new(&mut this.future_result)
            .poll(cx)
            .map_err(|_| BgTaskPanicked)
    }
}

/// A cloneable status of a future spawned with [`RemoteRuntime::spawn`] or
/// [`BgTaskRuntime::spawn`].
#[derive(Clone)]
pub struct BgTaskStatus {
    task_name: &'static str,
    result: Shared<oneshot::Receiver<Result<(), Arc<dyn Error + Send + Sync>>>>,
}

impl BgTaskStatus {
    /// Waits for the future to finish and returns its result.
    ///
    /// Should the future fail or panic, this function will return
    /// [`AgentError::BackgroundTaskFailed`].
    pub async fn wait(&self) -> Result<(), AgentError> {
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
    /// not expected to finish yet.
    pub async fn wait_assert_running(&self) -> AgentError {
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

impl fmt::Debug for BgTaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BgTaskStatus")
            .field("task_name", &self.task_name)
            .field("result", &self.result.clone().now_or_never())
            .finish()
    }
}

/// Convenience trait for transforming [`BgTask`] into [`BgTaskStatus`].
pub trait IntoStatus {
    fn into_status(self, task_name: &'static str) -> BgTaskStatus;
}

impl<E> IntoStatus for BgTask<Result<(), E>>
where
    E: Error + Send + Sync + 'static,
{
    fn into_status(self, task_name: &'static str) -> BgTaskStatus {
        let (result_tx, result_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = match self.future_result.await {
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

impl IntoStatus for BgTask<()> {
    fn into_status(self, task_name: &'static str) -> BgTaskStatus {
        let (result_tx, result_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = match self.future_result.await {
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

/// A runtime to spawn tasks on, either remote or local.
///
/// This can be used to spawn tasks that can either run in the target's namespace or the agent's.
///
/// If the agent has a target, you should use [`BgTaskRuntime::Remote`].
/// If the agent does not have a target, you should fallback to [`BgTaskRuntime::Local`].
#[derive(Clone)]
pub enum BgTaskRuntime {
    /// Remote runtime, which runs in the target's namespace.
    Remote(RemoteRuntime),
    /// Local runtime ([`tokio::runtime::Handle::current`]).
    Local,
}

impl BgTaskRuntime {
    /// Spawns the given future on this runtime.
    pub fn spawn<F>(&self, future: F) -> BgTask<F::Output>
    where
        F: 'static + Future + Send,
        F::Output: 'static + Send,
    {
        match self {
            Self::Remote(remote_runtime) => remote_runtime.spawn(future),
            Self::Local => {
                let (result_tx, result_rx) = oneshot::channel();

                tokio::spawn(async move {
                    let result = future.await;
                    let _ = result_tx.send(result);
                });

                BgTask {
                    future_result: result_rx,
                }
            }
        }
    }

    /// If this is a remote runtime, returns the target's PID.
    /// Otherwise, returns [`None`].
    pub fn target_pid(&self) -> Option<u64> {
        match self {
            Self::Remote(remote_runtime) => Some(remote_runtime.target_pid()),
            Self::Local => None,
        }
    }
}
