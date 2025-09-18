use std::{sync::Arc, thread};

use tokio::{
    runtime::Handle,
    sync::{Notify, oneshot},
};
use tracing::Level;

use crate::{
    namespace::{self, NamespaceType},
    util::error::AgentRuntimeError,
};

pub(super) mod status;

/// Runtime for the agent background tasks, such as: `BackgroundTask<SnifferCommand>`,
/// `BackgroundTask<StealerCommand>`, `BackgroundTask<DnsCommand>`.
///
/// Use [`BgTaskRuntime::spawn`] to create a new [`tokio::runtime::Runtime`], and you can spawn
/// tasks on it by calling [`tokio::runtime::Runtime::spawn`] on [`BgTaskRuntime::handle()`].
///
/// **Attention**: Keep the runtime alive! In the [`Drop`] impl we call
/// [`Notify::notify_one`] which will end the `BackgroundTask`. If you see an error like
/// `BackgroundTaskFailed { task: "TcpSnifferTask", error: BgTaskPanicked }`, this means that
/// the runtime was dropped before the `BackgroundTask` could start.
///
/// We keep the client connection (main loop) runtime separate from the background tasks to avoid
/// potentially blocking the agent from exiting if some background task hangs. When the agent
/// doesn't exit properly (i.e. it stays alive in the cluster, and the user has to manually kill
/// it), we cannot start another agent due to the dirty iptables check.
#[derive(Clone)]
pub(crate) struct BgTaskRuntime {
    target_pid: Option<u64>,
    handle: Handle,
    notify_drop: Arc<Notify>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeNamespace {
    target_pid: u64,
    namespace_type: NamespaceType,
}

impl BgTaskRuntime {
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub(crate) async fn spawn(
        namespace: Option<RuntimeNamespace>,
    ) -> Result<Self, AgentRuntimeError> {
        let (result_tx, result_rx) = oneshot::channel();
        let notify_drop = Arc::new(Notify::new());
        let notify_cloned = notify_drop.clone();

        let thread_logic = move || {
            tracing::info!("thread_logic ->");
            if let Some(RuntimeNamespace {
                target_pid,
                namespace_type,
            }) = namespace
                && let Err(error) = namespace::set_namespace(target_pid, namespace_type)
            {
                let _ = result_tx.send(Err(error.into()));
                return;
            }

            let rt_result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            tracing::info!("thread_logic -> rt_result {rt_result:?}");
            let rt = match rt_result {
                Ok(rt) => rt,
                Err(error) => {
                    let _ = result_tx.send(Err(AgentRuntimeError::TokioRuntimeError(error)));
                    return;
                }
            };

            tracing::info!("thread_logic -> result_tx.send");
            if result_tx.send(Ok(rt.handle().clone())).is_err() {
                return;
            }

            tracing::info!("thread_logic -> rt.block_on");
            rt.block_on(notify_cloned.notified());
        };

        thread::Builder::new()
            .spawn(thread_logic)
            .map_err(AgentRuntimeError::ThreadSpawnError)?;

        match result_rx.await {
            Ok(Ok(handle)) => Ok(Self {
                target_pid: namespace.map(|namespace| namespace.target_pid),
                handle,
                notify_drop,
            }),
            Ok(Err(error)) => {
                tracing::info!("something went wrong on ok {error:?}");
                Err(error)
            }
            Err(fail) => {
                tracing::info!("here we just panic {fail:?}");

                Err(AgentRuntimeError::Panicked)
            }
        }
    }

    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }

    pub(crate) fn target_pid(&self) -> Option<u64> {
        self.target_pid
    }
}

impl Drop for BgTaskRuntime {
    #[tracing::instrument(level = Level::DEBUG)]
    fn drop(&mut self) {
        if self.handle.metrics().num_alive_tasks() == 0 {
            self.notify_drop.notify_one();
        }
    }
}

impl core::fmt::Debug for BgTaskRuntime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let metrics = self.handle.metrics();

        f.debug_struct("BgTaskRuntime")
            .field("target_pid", &self.target_pid)
            .field("num_workers", &metrics.num_workers())
            .field("num_alive_tasks", &metrics.num_alive_tasks())
            .finish()
    }
}

impl RuntimeNamespace {
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub(crate) fn new(target_pid: u64, namespace_type: NamespaceType) -> Self {
        Self {
            target_pid,
            namespace_type,
        }
    }
}
