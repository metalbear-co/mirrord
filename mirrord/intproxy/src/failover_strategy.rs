use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::Duration,
};

use mirrord_intproxy_protocol::{
    IncomingRequest, LayerId, LayerToProxyMessage, LocalMessage, MessageId, ProcessInfo,
    ProxyToLayerMessage,
};
use mirrord_protocol::FileRequest;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::{
    IntProxy, LayerInitializerTask,
    background_tasks::{BackgroundTasks, TaskError, TaskSender, TaskUpdate},
    error::{ProxyRuntimeError, ProxyStartupError},
    layer_conn::LayerConnection,
    main_tasks::{FromLayer, MainTaskId, ProxyMessage},
    process_termination, quiesce_and_collect_layers,
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
    layer_initializer: LayerInitializerTask,
    layers: HashMap<LayerId, TaskSender<LayerConnection>>,
    pending_layers: Vec<(LayerId, MessageId)>,
    any_connection_accepted: bool,
    fail_cause: ProxyRuntimeError,
    /// Processes of the layers currently connected while the proxy is in failover.
    ///
    /// Only a terminal, non-recoverable agent failure reaches failover (a reconnectable session
    /// reconnects inside [`AgentConnection`](crate::agent_conn::AgentConnection) instead). Once
    /// here, the failure has broken every mirrord-hooked path in these processes. Tracking both
    /// the layers inherited from the failed proxy and layers accepted during failover lets every
    /// shutdown path tear them down instead of leaving silent zombies that keep holding their
    /// ports. See [`Self::terminate_connected_processes`].
    connected_layers: HashMap<LayerId, ProcessInfo>,
}

impl FailoverStrategy {
    fn has_layer_connections(&self) -> bool {
        !self.layers.is_empty()
    }

    pub fn from_failed_proxy(failed_proxy: IntProxy, error: ProxyRuntimeError) -> Self {
        FailoverStrategy {
            background_tasks: failed_proxy.background_tasks,
            layer_initializer: failed_proxy.task_txs.layer_initializer,
            layers: failed_proxy.task_txs.layers,
            pending_layers: failed_proxy.pending_layers.into_iter().collect(),
            any_connection_accepted: failed_proxy.any_connection_accepted,
            fail_cause: error,
            connected_layers: failed_proxy.connected_layers,
        }
    }

    /// Collects every process mirrord is loaded into for failover-entry termination.
    ///
    /// `mirrord exec` replaces the CLI with the user binary via `execv`, so once a session is
    /// running the intproxy is the only mirrord-controlled process left that observes the agent
    /// dropping. The user binary only finds out lazily, on its next hooked syscall; a process idle
    /// in `accept()` never makes that call once the agent goes away and no more traffic arrives, so
    /// it hangs forever as a zombie holding its ports. Rather than fail silently, we terminate
    /// every connected process so the failure is loud and nothing lingers.
    ///
    /// See [`process_termination::terminate_processes`] for the per-platform termination.
    fn connected_processes(&self) -> HashSet<i32> {
        if self.connected_layers.is_empty() {
            return HashSet::new();
        }

        let processes = self
            .connected_layers
            .values()
            .map(|info| (info.pid, info.name.as_str()))
            .collect::<Vec<_>>();

        tracing::warn!(
            ?processes,
            cause = %self.fail_cause,
            "Agent connection was lost and cannot be recovered. Terminating every injected \
             process, as every mirrord-hooked path in them is now broken.",
        );

        processes.into_iter().map(|(pid, _)| pid).collect()
    }

    pub async fn run(
        self,
        first_timeout: Duration,
        idle_timeout: Duration,
        shutdown: &CancellationToken,
    ) -> Result<(), ProxyStartupError> {
        self.run_with_termination(
            first_timeout,
            idle_timeout,
            shutdown,
            process_termination::terminate_processes,
        )
        .await
    }

    async fn run_with_termination<Terminate, Termination>(
        self,
        first_timeout: Duration,
        idle_timeout: Duration,
        shutdown: &CancellationToken,
        mut terminate_processes: Terminate,
    ) -> Result<(), ProxyStartupError>
    where
        Terminate: FnMut(HashSet<i32>) -> Termination,
        Termination: Future<Output = ()>,
    {
        let mut failover = self;

        while let Some((layer_id, message_id)) = failover.pending_layers.pop() {
            failover.send_error_to_layer(layer_id, message_id).await;
        }

        // Cancellation must not restart an inherited PID's fixed TERM/KILL sequence: apart from
        // adding a second grace period, a reused numeric PID could then target another process.
        let mut terminated_pids = HashSet::new();
        if !shutdown.is_cancelled() {
            let inherited_pids = failover.connected_processes();
            if !inherited_pids.is_empty() {
                terminate_processes(inherited_pids.clone()).await;
                terminated_pids.extend(inherited_pids);
            }
        }

        let mut shutdown_error = None;
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    let mut shutdown_layers = quiesce_and_collect_layers(
                        &mut failover.background_tasks,
                        &mut failover.layer_initializer,
                        &failover.connected_layers,
                    )
                    .await;
                    shutdown_layers
                        .pids
                        .retain(|pid| !terminated_pids.contains(pid));
                    if !shutdown_layers.pids.is_empty() {
                        terminate_processes(shutdown_layers.pids.clone()).await;
                    }
                    shutdown_error = shutdown_layers.error.take();
                    std::mem::drop(shutdown_layers);
                    break;
                },
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

        match shutdown_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn handle_task_update(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, ProxyRuntimeError>,
    ) {
        if task_id == MainTaskId::LayerInitializer && matches!(&update, TaskUpdate::Finished(_)) {
            self.layer_initializer.finished = true;
        }

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
                self.connected_layers.remove(&LayerId(id));
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
                let layer_id = new_layer.id;
                self.connected_layers
                    .insert(layer_id, new_layer.process_info);
                let tx = self.background_tasks.register(
                    LayerConnection::new(new_layer.stream, layer_id),
                    MainTaskId::LayerConnection(layer_id),
                    IntProxy::CHANNEL_SIZE,
                );
                self.layers.insert(layer_id, tx);
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

#[cfg(all(test, unix))]
mod tests {
    use std::{
        collections::HashSet,
        net::SocketAddr,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures::{SinkExt, StreamExt};
    use mirrord_config::{config::MirrordConfig, experimental::ExperimentalFileConfig};
    use mirrord_intproxy_protocol::{
        LayerId, LayerToProxyMessage, LocalMessage, NewSessionRequest, ProcessInfo,
        ProxyToLayerMessage,
    };
    use mirrord_protocol_io::Connection;
    use nix::sys::signal::Signal;
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::{Notify, watch},
    };
    use tokio_util::sync::CancellationToken;

    use super::FailoverStrategy;
    use crate::{
        IntProxy, IntProxyIntervals,
        agent_conn::{AgentConnectInfoDiscriminants, AgentConnection, ReconnectFlow},
        error::ProxyRuntimeError,
        layer_initializer::RegistrationGateControl,
        session_monitor::{MonitorTx, chaos::ChaosWatcherRx},
    };

    const INHERITED_PID: i32 = 101;
    const QUEUED_PID: i32 = 202;

    async fn make_failover(
        inherited_pids: impl IntoIterator<Item = i32>,
    ) -> (FailoverStrategy, SocketAddr, RegistrationGateControl) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let (connection, _proxy_tx, _proxy_rx) = Connection::dummy();
        let agent_conn = AgentConnection {
            connection,
            reconnect: ReconnectFlow::Break(AgentConnectInfoDiscriminants::DirectKubernetes),
        };
        let (_, chaos_rx) = watch::channel(Default::default());
        let mut proxy = IntProxy::new_with_connection(
            agent_conn,
            listener,
            4096,
            Default::default(),
            IntProxyIntervals {
                ping: IntProxy::PING_INTERVAL,
                process_logging: Duration::from_secs(60),
            },
            &ExperimentalFileConfig::default()
                .generate_config(&mut Default::default())
                .unwrap(),
            MonitorTx::disabled(),
            ChaosWatcherRx::new(chaos_rx),
        );

        for (index, pid) in inherited_pids.into_iter().enumerate() {
            proxy.connected_layers.insert(
                LayerId(index as u64),
                ProcessInfo {
                    pid,
                    parent_pid: 1,
                    name: format!("inherited-{pid}"),
                    cmdline: Vec::new(),
                    loaded: true,
                },
            );
        }

        let registration_gate = proxy
            .task_txs
            .layer_initializer
            .shutdown
            .registration_gate();
        let failover = FailoverStrategy::from_failed_proxy(
            proxy,
            ProxyRuntimeError::AgentFailed("test failure".to_owned()),
        );

        (failover, proxy_addr, registration_gate)
    }

    fn record_signals(pids: &HashSet<i32>, signal: Signal, signals: &Mutex<Vec<(i32, Signal)>>) {
        signals
            .lock()
            .unwrap()
            .extend(pids.iter().map(|pid| (*pid, signal)));
    }

    fn assert_one_signal_pair(signals: &[(i32, Signal)], pid: i32) {
        assert_eq!(
            signals
                .iter()
                .filter(|event| **event == (pid, Signal::SIGTERM))
                .count(),
            1,
        );
        assert_eq!(
            signals
                .iter()
                .filter(|event| **event == (pid, Signal::SIGKILL))
                .count(),
            1,
        );
    }

    /// Cancellation already visible at failover entry uses the quiesced shutdown path once rather
    /// than first terminating the inherited set and then targeting it again.
    #[tokio::test]
    async fn shutdown_terminates_registered_layer_outside_process_group_failover_pre_cancelled_once()
     {
        let (failover, _proxy_addr, _registration_gate) = make_failover([INHERITED_PID]).await;
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let signals = Arc::new(Mutex::new(Vec::new()));

        tokio::time::timeout(
            Duration::from_secs(5),
            failover.run_with_termination(
                Duration::from_secs(60),
                Duration::from_secs(60),
                &shutdown,
                {
                    let invocations = invocations.clone();
                    let signals = signals.clone();
                    move |pids| {
                        let invocations = invocations.clone();
                        let signals = signals.clone();
                        async move {
                            invocations.lock().unwrap().push(pids.clone());
                            record_signals(&pids, Signal::SIGTERM, &signals);
                            record_signals(&pids, Signal::SIGKILL, &signals);
                        }
                    }
                },
            ),
        )
        .await
        .expect("pre-cancelled failover did not shut down")
        .unwrap();

        let invocations = invocations.lock().unwrap();
        assert_eq!(&*invocations, &[HashSet::from([INHERITED_PID])]);
        assert!(invocations.iter().all(|pids| !pids.is_empty()));
        let signals = signals.lock().unwrap();
        assert_one_signal_pair(&signals, INHERITED_PID);
        assert_eq!(signals.len(), 2);
    }

    /// Cancellation wins over an already-ready failover timeout and retains a decoded
    /// registration whose producer is gated until quiescing has started.
    #[tokio::test]
    async fn shutdown_terminates_registered_layer_outside_process_group_failover_ready_timeout_registration_race()
     {
        let (failover, proxy_addr, registration_gate) = make_failover([]).await;
        registration_gate.pause();
        let conn = TcpStream::connect(proxy_addr).await.unwrap();
        let (mut encoder, mut decoder) = mirrord_intproxy_protocol::codec::make_async_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(conn);
        encoder
            .send(LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::NewSession(NewSessionRequest {
                    process_info: ProcessInfo {
                        pid: QUEUED_PID,
                        parent_pid: 1,
                        name: "ready-timeout-layer".to_owned(),
                        cmdline: Vec::new(),
                        loaded: true,
                    },
                    parent_layer: None,
                }),
            })
            .await
            .unwrap();
        assert!(matches!(
            decoder.next().await.unwrap().unwrap(),
            LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::NewSession(_),
            }
        ));
        registration_gate.wait_until_reached().await;

        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let failover_handle = tokio::spawn({
            let invocations = invocations.clone();
            async move {
                failover
                    .run_with_termination(Duration::ZERO, Duration::ZERO, &shutdown, move |pids| {
                        let invocations = invocations.clone();
                        async move {
                            invocations.lock().unwrap().push(pids);
                        }
                    })
                    .await
            }
        });
        registration_gate.wait_for_shutdown_request().await;
        registration_gate.release();

        tokio::time::timeout(Duration::from_secs(5), failover_handle)
            .await
            .expect("ready timeout bypassed failover quiescing")
            .unwrap()
            .unwrap();
        assert_eq!(
            invocations.lock().unwrap().as_slice(),
            &[HashSet::from([QUEUED_PID])]
        );

        std::mem::drop((encoder, decoder));
    }

    /// A registration queued while the inherited set is in its fixed termination sequence is
    /// drained after cancellation, and the two disjoint sets are each signalled exactly once.
    #[tokio::test]
    async fn shutdown_terminates_registered_layer_outside_process_group_failover_queued_registration_once()
     {
        let (failover, proxy_addr, registration_gate) = make_failover([INHERITED_PID]).await;
        registration_gate.pause();
        let shutdown = CancellationToken::new();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let signals = Arc::new(Mutex::new(Vec::new()));
        let inherited_term_sent = Arc::new(Notify::new());
        let finish_inherited_grace = Arc::new(Notify::new());

        let failover_handle = tokio::spawn({
            let shutdown = shutdown.clone();
            let invocations = invocations.clone();
            let signals = signals.clone();
            let inherited_term_sent = inherited_term_sent.clone();
            let finish_inherited_grace = finish_inherited_grace.clone();
            async move {
                failover
                    .run_with_termination(
                        Duration::from_secs(60),
                        Duration::from_secs(60),
                        &shutdown,
                        move |pids| {
                            let invocations = invocations.clone();
                            let signals = signals.clone();
                            let inherited_term_sent = inherited_term_sent.clone();
                            let finish_inherited_grace = finish_inherited_grace.clone();
                            async move {
                                invocations.lock().unwrap().push(pids.clone());
                                record_signals(&pids, Signal::SIGTERM, &signals);
                                if pids.contains(&INHERITED_PID) {
                                    inherited_term_sent.notify_one();
                                    finish_inherited_grace.notified().await;
                                }
                                record_signals(&pids, Signal::SIGKILL, &signals);
                            }
                        },
                    )
                    .await
            }
        });

        inherited_term_sent.notified().await;
        let conn = TcpStream::connect(proxy_addr).await.unwrap();
        let (mut encoder, mut decoder) = mirrord_intproxy_protocol::codec::make_async_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(conn);
        encoder
            .send(LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::NewSession(NewSessionRequest {
                    process_info: ProcessInfo {
                        pid: QUEUED_PID,
                        parent_pid: 1,
                        name: "queued-layer".to_owned(),
                        cmdline: Vec::new(),
                        loaded: true,
                    },
                    parent_layer: None,
                }),
            })
            .await
            .unwrap();
        assert!(matches!(
            decoder.next().await.unwrap().unwrap(),
            LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::NewSession(_),
            }
        ));
        registration_gate.wait_until_reached().await;

        shutdown.cancel();
        finish_inherited_grace.notify_one();
        registration_gate.wait_for_shutdown_request().await;
        registration_gate.release();

        tokio::time::timeout(Duration::from_secs(5), failover_handle)
            .await
            .expect("failover did not finish after draining the queued registration")
            .unwrap()
            .unwrap();

        let invocations = invocations.lock().unwrap();
        assert_eq!(
            invocations.as_slice(),
            &[HashSet::from([INHERITED_PID]), HashSet::from([QUEUED_PID]),]
        );
        assert!(invocations.iter().all(|pids| !pids.is_empty()));
        let signals = signals.lock().unwrap();
        assert_one_signal_pair(&signals, INHERITED_PID);
        assert_one_signal_pair(&signals, QUEUED_PID);
        assert_eq!(signals.len(), 4);

        std::mem::drop((encoder, decoder));
    }
}
