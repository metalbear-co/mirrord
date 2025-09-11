use std::thread;

use futures::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};
use tracing::Level;

use super::error::AgentRuntimeError;
use crate::util::remote_runtime::RuntimeSpawn;

#[derive(Clone)]
pub struct LocalRuntime {
    /// `Some(pid)` when we're running ephemeral, and `None` when running `targetless`.
    target_pid: Option<u64>,
    future_tx: mpsc::Sender<BoxFuture<'static, ()>>,
}

impl RuntimeSpawn for LocalRuntime {
    fn future_tx(&self) -> mpsc::Sender<BoxFuture<'static, ()>> {
        self.future_tx.clone()
    }
}

impl LocalRuntime {
    #[tracing::instrument(level = Level::INFO, err)]
    pub async fn new(target_pid: Option<u64>) -> Result<Self, AgentRuntimeError> {
        let pid = target_pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "no-pid".to_string());

        let (future_tx, mut future_rx) = mpsc::channel(16);
        let (result_tx, result_rx) = oneshot::channel();
        let thread_name = format!("local-{pid}-runtime-thread");
        let thread_logic = move || {
            let rt_result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_name(format!("local-{pid}-runtime-worker"))
                .build();
            let rt = match rt_result {
                Ok(rt) => rt,
                Err(error) => {
                    let _ = result_tx.send(Err(AgentRuntimeError::TokioRuntimeError(error)));
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
            .map_err(AgentRuntimeError::ThreadSpawnError)?;

        match result_rx.await {
            Ok(Ok(())) => Ok(Self {
                target_pid,
                future_tx,
            }),
            Ok(Err(error)) => Err(error),
            Err(..) => Err(AgentRuntimeError::Panicked),
        }
    }

    /// Returns the target's PID.
    pub fn target_pid(&self) -> Option<u64> {
        self.target_pid
    }
}
