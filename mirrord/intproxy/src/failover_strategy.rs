use std::{collections::HashMap, time::Duration};

use mirrord_intproxy_protocol::{
    IncomingRequest, LayerId, LayerToProxyMessage, LocalMessage, MessageId, ProcessInfo,
    ProxyToLayerMessage,
};
use mirrord_protocol::FileRequest;
#[cfg(unix)]
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tokio::time;
#[cfg(windows)]
use winapi::{
    shared::minwindef::FALSE,
    um::{
        handleapi::CloseHandle,
        processthreadsapi::{OpenProcess, TerminateProcess},
        winnt::PROCESS_TERMINATE,
    },
};

use crate::{
    IntProxy,
    background_tasks::{BackgroundTasks, TaskError, TaskSender, TaskUpdate},
    error::{ProxyRuntimeError, ProxyStartupError},
    layer_conn::LayerConnection,
    layer_initializer::LayerInitializer,
    main_tasks::{FromLayer, MainTaskId, ProxyMessage},
};

/// This struct is a strategy that handle failover logic for [`IntProxy`].
///
/// Essentially send an error message to every layer that:
/// - is waiting for a response and wasn't yet updated bout the failure happened
/// - send an error to every layer that sends a message (if a response is expected in the normal
///   workflow)
///
/// All while continues to accept new connections from layers
pub(super) struct FailoverStrategy {
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
    layer_initializer: TaskSender<LayerInitializer>,
    layers: HashMap<LayerId, TaskSender<LayerConnection>>,
    pending_layers: Vec<(LayerId, MessageId)>,
    any_connection_accepted: bool,
    fail_cause: ProxyRuntimeError,
    /// Processes of the layers still connected when the proxy failed.
    ///
    /// Only a terminal, non-recoverable agent failure reaches failover (a reconnectable session
    /// reconnects inside [`AgentConnection`](crate::agent_conn::AgentConnection) instead). Once
    /// here, the failure has broken every mirrord-hooked path in these processes, so we tear them
    /// down instead of leaving silent zombies that keep holding their ports. See
    /// [`Self::terminate_connected_processes`].
    connected_layers: HashMap<LayerId, ProcessInfo>,
}

/// Grace period between the `SIGTERM` and the `SIGKILL` we send to the meshed processes on a
/// terminal failure, giving them a chance to run their own shutdown before we force the issue.
/// Shortened under test so tests can exercise the real termination path without a slow wait.
#[cfg(all(unix, not(test)))]
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
#[cfg(all(unix, test))]
const TERMINATION_GRACE: Duration = Duration::from_millis(50);

impl FailoverStrategy {
    fn has_layer_connections(&self) -> bool {
        !self.layers.is_empty()
    }

    pub fn from_failed_proxy(failed_proxy: IntProxy, error: ProxyRuntimeError) -> Self {
        FailoverStrategy {
            background_tasks: failed_proxy.background_tasks,
            layer_initializer: failed_proxy.task_txs._layer_initializer,
            layers: failed_proxy.task_txs.layers,
            pending_layers: failed_proxy.pending_layers.into_iter().collect(),
            any_connection_accepted: failed_proxy.any_connection_accepted,
            fail_cause: error,
            connected_layers: failed_proxy.connected_layers,
        }
    }

    /// Tears down the processes this proxy meshed.
    ///
    /// `mirrord exec` replaces the CLI with the user binary via `execv`, so once a session is
    /// running the intproxy is the only mirrord-controlled process left that observes the agent
    /// dropping. The user binary only finds out lazily, on its next hooked syscall; a process idle
    /// in `accept()` never makes that call once the agent goes away and no more traffic arrives, so
    /// it hangs forever as a zombie holding its ports. Rather than fail silently, we terminate
    /// every connected process so the failure is loud and nothing lingers.
    ///
    /// See [`Self::signal_processes`] for the per-platform termination.
    async fn terminate_connected_processes(&self) {
        let pids = self
            .connected_layers
            .values()
            .map(|info| {
                tracing::error!(
                    pid = info.pid,
                    process = info.name,
                    cause = %self.fail_cause,
                    "Agent connection was lost and cannot be recovered. Terminating the meshed \
                     process, as every mirrord-hooked path in it is now broken.",
                );
                info.pid
            })
            .collect::<Vec<_>>();

        Self::signal_processes(pids).await;
    }

    /// On unix, sends `SIGTERM` to the given processes, then `SIGKILL` to any survivors after
    /// [`TERMINATION_GRACE`], so well-behaved processes get to run their shutdown first.
    #[cfg(unix)]
    async fn signal_processes(pids: Vec<i32>) {
        if pids.is_empty() {
            return;
        }

        for pid in &pids {
            // An invalid or already-reaped pid just yields `ESRCH`, which we ignore.
            let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        }

        time::sleep(TERMINATION_GRACE).await;

        for pid in pids {
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }

    /// On Windows, calls `TerminateProcess` on each pid. No reliable graceful signal exists for an
    /// arbitrary process here, so this matches the unix `SIGKILL` with no grace phase.
    #[cfg(windows)]
    async fn signal_processes(pids: Vec<i32>) {
        for pid in pids {
            // SAFETY: FFI. An invalid or exited pid yields a null handle, which we skip; every
            // opened handle is closed. `TerminateProcess` on a valid handle cannot fail us into UB.
            unsafe {
                let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid as u32);
                if handle.is_null() {
                    continue;
                }
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }

    pub async fn run(
        self,
        first_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<(), ProxyStartupError> {
        let mut failover = self;

        while let Some((layer_id, message_id)) = failover.pending_layers.pop() {
            failover.send_error_to_layer(layer_id, message_id).await;
        }

        failover.terminate_connected_processes().await;

        loop {
            tokio::select! {
                Some((task_id, task_update)) = failover.background_tasks.next() => {
                    tracing::trace!(
                        %task_id,
                        ?task_update,
                        "Received a task update",
                    );
                    failover.handle_task_update(task_id, task_update).await;
                }
                _ = time::sleep(first_timeout), if !failover.any_connection_accepted => {
                    Err(ProxyStartupError::ConnectionAcceptTimeout)?;
                },
                _ = time::sleep(idle_timeout), if failover.any_connection_accepted && !failover.has_layer_connections() => {
                    tracing::info!("Reached the idle timeout with no active layer connections");
                    break;
                },
            }
        }

        std::mem::drop(failover.layer_initializer);
        std::mem::drop(failover.layers);

        tracing::info!("Collecting background task results before exiting");
        let results = failover.background_tasks.results().await;

        for (task_id, result) in results {
            tracing::trace!(
                %task_id,
                ?result,
                "Collected a background task result",
            );
        }

        Ok(())
    }

    async fn handle_task_update(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, ProxyRuntimeError>,
    ) {
        match (task_id, update) {
            (MainTaskId::LayerConnection(LayerId(id)), TaskUpdate::Finished(result)) => {
                match result {
                    Ok(()) => {
                        tracing::info!(layer_id = id, "Layer connection closed");
                    }
                    Err(error) => {
                        tracing::error!(layer_id = id, %error, "Layer connection failed");
                    }
                }
                self.layers.remove(&LayerId(id));
            }
            (task_id, TaskUpdate::Finished(res)) => match res {
                Ok(()) => {
                    tracing::error!(%task_id, "One of the main tasks finished unexpectedly");
                }
                Err(TaskError::Error(error)) => {
                    tracing::error!(%task_id, %error, "One of the main tasks failed");
                }
                Err(TaskError::Panic) => {
                    tracing::error!(%task_id, "One of the main tasks panicked");
                }
            },

            (_, TaskUpdate::Message(msg)) => self.handle(msg).await,
        }
    }

    async fn handle(&mut self, msg: ProxyMessage) {
        match msg {
            ProxyMessage::NewLayer(new_layer) => {
                self.any_connection_accepted = true;
                let tx = self.background_tasks.register(
                    LayerConnection::new(new_layer.stream, new_layer.id),
                    MainTaskId::LayerConnection(new_layer.id),
                    IntProxy::CHANNEL_SIZE,
                );
                self.layers.insert(new_layer.id, tx);
            }
            ProxyMessage::FromLayer(message) => {
                self.update_layer_on_error(message).await;
            }
            msg => {
                tracing::info!(message = ?msg, "Proxy in failover mode, ignoring a message");
            }
        }
    }

    async fn update_layer_on_error(
        &self,
        FromLayer {
            layer_id,
            message_id,
            message,
        }: FromLayer,
    ) {
        match message {
            LayerToProxyMessage::File(FileRequest::Close(_) | FileRequest::CloseDir(_))
            | LayerToProxyMessage::Incoming(IncomingRequest::PortUnsubscribe(_)) => {
                tracing::info!(message = ?message, "Proxy in failover mode, ignoring a message");
            }
            _ => self.send_error_to_layer(layer_id, message_id).await,
        }
    }

    async fn send_error_to_layer(&self, layer_id: LayerId, message_id: MessageId) {
        match self.layers.get(&layer_id) {
            Some(layer) => {
                layer
                    .send(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::ProxyFailed {
                            agent_reported: self.fail_cause.is_agent_reported(),
                            message: self.fail_cause.to_string(),
                        },
                    })
                    .await;
            }
            _ => {
                tracing::warn!(
                    "Layer {:?} not found, but it was waiting for proxy to respond!",
                    layer_id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, time::Duration};

    use super::FailoverStrategy;

    /// Spawns a real, long-lived child process for the current platform.
    fn spawn_blocking_child() -> std::process::Child {
        #[cfg(unix)]
        {
            Command::new("sleep").arg("30").spawn().unwrap()
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "ping", "-n", "30", "127.0.0.1"])
                .spawn()
                .unwrap()
        }
    }

    /// [`FailoverStrategy::signal_processes`] must actually terminate the given processes on the
    /// platforms we support, not silently do nothing. Exercises the real (per-platform) kill path.
    #[tokio::test]
    async fn signal_processes_terminates_the_given_pids() {
        let mut child = spawn_blocking_child();
        let pid = child.id() as i32;

        FailoverStrategy::signal_processes(vec![pid]).await;

        let terminated = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("child process was not terminated by signal_processes");

        assert!(
            !terminated.success(),
            "child should have been killed, but it exited cleanly"
        );
    }
}
