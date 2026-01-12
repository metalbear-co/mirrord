#![feature(error_reporter)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::ControlFlow,
    time::Duration,
};

use background_tasks::{BackgroundTasks, TaskSender, TaskUpdate};
use error::UnexpectedAgentMessage;
use layer_conn::LayerConnection;
use layer_initializer::LayerInitializer;
use main_tasks::{FromLayer, LayerForked, MainTaskId, ProxyMessage, ToLayer};
use mirrord_config::{
    experimental::ExperimentalConfig, feature::network::incoming::tls_delivery::LocalTlsDelivery,
};
use mirrord_intproxy_protocol::{
    IncomingRequest, LayerId, LayerToProxyMessage, LocalMessage, MessageId, ProcessInfo,
};
use mirrord_protocol::{
    CLIENT_READY_FOR_LOGS, ClientMessage, DaemonMessage, FileRequest, LogLevel,
};
use mirrord_protocol_io::{Client, TxHandle};
use ping_pong::{PingPong, PingPongMessage};
use proxies::{
    files::{FilesProxy, FilesProxyMessage},
    incoming::{IncomingProxy, IncomingProxyMessage},
    outgoing::{OutgoingProxy, OutgoingProxyMessage},
    simple::{SimpleProxy, SimpleProxyMessage},
};
use semver::Version;
use tokio::{
    net::TcpListener,
    time,
    time::{Interval, MissedTickBehavior},
};

use crate::{
    agent_conn::{AgentConnection, AgentConnectionMessage},
    background_tasks::{RestartableBackgroundTaskWrapper, TaskError},
    error::{ProxyRuntimeError, ProxyStartupError},
    failover_strategy::FailoverStrategy,
    main_tasks::{ConnectionRefresh, LayerClosed},
};

pub mod agent_conn;
pub mod background_tasks;
pub mod error;
mod failover_strategy;
mod layer_conn;
mod layer_initializer;
pub mod main_tasks;
mod ping_pong;
pub mod proxies;
mod remote_resources;
mod request_queue;

/// [`TaskSender`]s for main background tasks. See [`MainTaskId`].
struct TaskTxs {
    layers: HashMap<LayerId, TaskSender<LayerConnection>>,
    _layer_initializer: TaskSender<LayerInitializer>,
    agent: TaskSender<RestartableBackgroundTaskWrapper<AgentConnection>>,
    simple: TaskSender<SimpleProxy>,
    ping_pong: TaskSender<RestartableBackgroundTaskWrapper<PingPong>>,
    outgoing: TaskSender<OutgoingProxy>,
    incoming: TaskSender<IncomingProxy>,
    files: TaskSender<FilesProxy>,
}

/// This struct contains logic for proxying between multiple layer instances and one agent.
/// It maintains a singe agent connection.
///
/// Utilizes multiple [`BackgroundTask`](background_tasks::BackgroundTask)s to split logic of
/// different mirrod features (e.g. file operations and incoming traffic).
pub struct IntProxy {
    any_connection_accepted: bool,
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
    task_txs: TaskTxs,

    /// this set holds the ids of current layer and msg involved in an exchange with proxy
    pending_layers: HashSet<(LayerId, MessageId)>,

    /// [`mirrord_protocol`] version negotiated with the agent.
    protocol_version: Option<Version>,

    /// Temporary message queue for any [`ProxyMessage`] from layer or to agent that are sent
    /// during reconnection state.
    reconnect_task_queue: Option<VecDeque<ProxyMessage>>,

    // Simple ping preset state-machine to debounce ping-pong resets (from agent activity) to at
    // most every 10/th of `PING_INTERVAL`
    ping_pong_update_debounce: Interval,
    ping_pong_update_allowed: bool,

    /// Connected layer process information for periodic logging
    connected_layers: HashMap<LayerId, ProcessInfo>,

    /// Interval for logging connected process information
    process_logging_interval: Interval,

    /// Send handle for the agent connection
    agent_tx: TxHandle<Client>,
}

impl IntProxy {
    /// Size of channels used to communicate with main tasks (see [`MainTaskId`]).
    const CHANNEL_SIZE: usize = 512;
    /// How long can the agent connection remain silent.
    #[cfg(not(test))]
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    #[cfg(test)]
    const PING_INTERVAL: Duration = Duration::from_secs(1);
    /// How many sequential reconnects should PingPong task attepmt to perform before giving up.
    const PING_PONG_MAX_RECONNECTS: usize = 5;

    /// Creates a new [`IntProxy`] using existing [`AgentConnection`].
    /// The returned instance will accept connections from the layers using the given
    /// [`TcpListener`].
    pub fn new_with_connection(
        agent_conn: AgentConnection,
        listener: TcpListener,
        file_buffer_size: u64,
        https_delivery: LocalTlsDelivery,
        process_logging_interval: Duration,
        experimental: &ExperimentalConfig,
    ) -> Self {
        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError> =
            BackgroundTasks::new(agent_conn.connection.tx_handle());

        let layer_initializer = background_tasks.register(
            LayerInitializer::new(listener),
            MainTaskId::LayerInitializer,
            Self::CHANNEL_SIZE,
        );

        let agent_conn_reconnectable = agent_conn.reconnectable();

        // We need to negotiate mirrord-protocol version
        // before we can process layers' requests.
        //
        // If we don't do this, we risk responding with `NotImplemented`
        // to requests that have a requirement on the mirrord-protocol version.
        background_tasks.suspend_messages(MainTaskId::LayerInitializer);
        let ping_pong = background_tasks.register_restartable(
            PingPong::new(
                Self::PING_INTERVAL,
                if agent_conn_reconnectable {
                    Self::PING_PONG_MAX_RECONNECTS
                } else {
                    0
                },
            ),
            MainTaskId::PingPong,
            Self::CHANNEL_SIZE,
        );
        let simple = background_tasks.register(
            SimpleProxy::new(experimental.dns_permission_error_fatal),
            MainTaskId::SimpleProxy,
            Self::CHANNEL_SIZE,
        );
        let outgoing = background_tasks.register(
            OutgoingProxy::new(experimental.non_blocking_tcp_connect),
            MainTaskId::OutgoingProxy,
            Self::CHANNEL_SIZE,
        );
        let incoming = background_tasks.register(
            IncomingProxy::new(
                Duration::from_millis(experimental.idle_local_http_connection_timeout),
                https_delivery,
            ),
            MainTaskId::IncomingProxy,
            Self::CHANNEL_SIZE,
        );
        let files = background_tasks.register(
            FilesProxy::new(file_buffer_size),
            MainTaskId::FilesProxy,
            Self::CHANNEL_SIZE,
        );

        let agent_tx = agent_conn.connection.tx_handle();

        let agent = background_tasks.register_restartable(
            agent_conn,
            MainTaskId::AgentConnection,
            Self::CHANNEL_SIZE,
        );

        let mut ping_pong_update_debounce = time::interval(Self::PING_INTERVAL / 10);
        ping_pong_update_debounce.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let mut process_logging_interval = time::interval(process_logging_interval);
        process_logging_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Self {
            any_connection_accepted: false,
            background_tasks,
            task_txs: TaskTxs {
                layers: Default::default(),
                _layer_initializer: layer_initializer,
                agent,
                simple,
                outgoing,
                incoming,
                ping_pong,
                files,
            },
            pending_layers: Default::default(),
            protocol_version: None,
            reconnect_task_queue: Default::default(),
            ping_pong_update_debounce,
            ping_pong_update_allowed: false,
            connected_layers: HashMap::new(),
            process_logging_interval,
            agent_tx,
        }
    }

    /// Check if any layer connections are still alive
    fn has_layer_connections(&self) -> bool {
        !self.task_txs.layers.is_empty()
    }

    /// Runs the main event loop till a failure or success happens, if the failure is manageable, it
    /// goes in failover state starting to update every layer with the error content on every new or
    /// pending task. In failover state it continues to accept connection from layers
    /// Expects to accept the first layer connection within the given `first_timeout`.
    /// Exits after `idle_timeout` when there are no more layer connections.
    pub async fn run(
        self,
        first_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<(), ProxyStartupError> {
        match self.run_inner(first_timeout, idle_timeout).await {
            ControlFlow::Break(result) => result,
            ControlFlow::Continue(failover_strategy) => {
                failover_strategy.run(idle_timeout, idle_timeout).await
            }
        }
    }

    /// Runs the main event loop of this proxy.
    ///
    /// Fails if the first layer connection is not accepted within the given `first_timeout`.
    /// Exits after `idle_timeout` when there are no more layer connections.
    ///
    /// # Returns
    ///
    /// 1. [`ControlFlow::Continue`] if a critical error was encountered. The [`FailoverStrategy`]
    ///    can be run in order to handle pending and future layer requests.
    /// 2. [`ControlFlow::Break`] if the proxy exited normally, or no layer connection was accepted
    ///    within the given `first_timeout`.
    async fn run_inner(
        self,
        first_timeout: Duration,
        idle_timeout: Duration,
    ) -> ControlFlow<Result<(), ProxyStartupError>, FailoverStrategy> {
        self.agent_tx
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        let mut proxy = self;

        loop {
            tokio::select! {
                Some((task_id, task_update)) = proxy.background_tasks.next() => {
                    tracing::trace!(
                        %task_id,
                        ?task_update,
                        "Received a task update",
                    );
                    if let Err(error) = proxy.handle_task_update(task_id, task_update).await {
                        tracing::error!(%error, "Proxy encountered a critical error, and is entering the failover state...");
                        return ControlFlow::Continue(FailoverStrategy::from_failed_proxy(proxy, error));
                    }
                }

                _ = proxy.ping_pong_update_debounce.tick(), if proxy.has_layer_connections() => {
                    proxy.ping_pong_update_allowed = true;
                }

                _ = proxy.process_logging_interval.tick() => {
                    // Always log this, even if there are no connected layers.
                    // This way we can be sure that intproxy's Tokio runtime is making progress.
                    tracing::info!(
                        count = proxy.connected_layers.len(),
                        layers = ?proxy.connected_layers,
                        "State of connected layers",
                    );
                }

                _ = time::sleep(first_timeout), if !proxy.any_connection_accepted => {
                    return ControlFlow::Break(Err(ProxyStartupError::ConnectionAcceptTimeout));
                },

                _ = time::sleep(idle_timeout), if proxy.any_connection_accepted && !proxy.has_layer_connections() => {
                    tracing::info!("Reached the idle timeout with no active layer connections");
                    break;
                },
            }
        }

        std::mem::drop(proxy.task_txs);

        tracing::info!("Collecting background task results before exiting");
        let results = proxy.background_tasks.results().await;

        for (task_id, result) in results {
            tracing::trace!(
                %task_id,
                ?result,
                "Collected a background task result",
            );
        }

        ControlFlow::Break(Ok(()))
    }

    /// Routes a [`ProxyMessage`] to the correct background task.
    /// [`ProxyMessage::NewLayer`] is handled here, as an exception.
    async fn handle(&mut self, msg: ProxyMessage) -> Result<(), ProxyRuntimeError> {
        match msg {
            ProxyMessage::NewLayer(_) | ProxyMessage::FromLayer(_)
                if self.reconnect_task_queue.is_some() =>
            {
                // We are in reconnect state so should queue this message.
                self.reconnect_task_queue
                    .as_mut()
                    .unwrap_or_else(|| {
                        tracing::error!("Unexpected state: reconnect_task_queue should contain a value when the proxy is in reconnect state");
                        panic!("reconnect_task_queue should contain value when in reconnect state")
                    })
                    .push_back(msg);
            }
            ProxyMessage::NewLayer(new_layer) => {
                self.any_connection_accepted = true;

                self.connected_layers
                    .insert(new_layer.id, new_layer.process_info);
                let tx = self.background_tasks.register(
                    LayerConnection::new(new_layer.stream, new_layer.id),
                    MainTaskId::LayerConnection(new_layer.id),
                    Self::CHANNEL_SIZE,
                );
                self.task_txs.layers.insert(new_layer.id, tx);

                if let Some(parent) = new_layer.parent_id {
                    let msg = LayerForked {
                        child: new_layer.id,
                        parent,
                    };

                    self.task_txs
                        .files
                        .send(FilesProxyMessage::LayerForked(msg))
                        .await;
                    self.task_txs
                        .incoming
                        .send(IncomingProxyMessage::LayerForked(msg))
                        .await;
                    self.task_txs
                        .outgoing
                        .send(OutgoingProxyMessage::LayerForked(msg))
                        .await;
                }
            }
            ProxyMessage::FromAgent(msg) => self.handle_agent_message(msg).await?,
            ProxyMessage::FromLayer(msg) => {
                if !matches!(
                    msg.message,
                    LayerToProxyMessage::File(FileRequest::Close(_) | FileRequest::CloseDir(_))
                        | LayerToProxyMessage::Incoming(IncomingRequest::PortUnsubscribe(_))
                ) {
                    self.pending_layers.insert((msg.layer_id, msg.message_id));
                }
                self.handle_layer_message(msg).await?
            }
            ProxyMessage::ToLayer(msg) => {
                let ToLayer {
                    message,
                    message_id,
                    layer_id,
                } = msg;
                self.pending_layers.remove(&(layer_id, message_id));
                if let Some(tx) = self.task_txs.layers.get(&layer_id) {
                    tx.send(LocalMessage {
                        message_id,
                        inner: message,
                    })
                    .await;
                }
            }
            ProxyMessage::ConnectionRefresh(kind) => self.handle_connection_refresh(kind).await?,
        }

        Ok(())
    }

    /// Handles a [`TaskUpdate`] from one of the main tasks (see [`MainTaskId`]).
    async fn handle_task_update(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, ProxyRuntimeError>,
    ) -> Result<(), ProxyRuntimeError> {
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

                let msg = LayerClosed { id: LayerId(id) };

                self.task_txs
                    .files
                    .send(FilesProxyMessage::LayerClosed(msg))
                    .await;
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::LayerClosed(msg))
                    .await;
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::LayerClosed(msg))
                    .await;

                self.task_txs.layers.remove(&LayerId(id));
                self.connected_layers.remove(&LayerId(id));
                self.pending_layers.retain(|(layer_id, _)| layer_id.0 != id);
            }

            (task_id, TaskUpdate::Finished(res)) => match res {
                Ok(()) => {
                    tracing::error!(%task_id, "One of the main tasks finished unexpectedly");
                    Err(ProxyRuntimeError::TaskExit(task_id))?;
                }
                Err(TaskError::Error(error)) => {
                    tracing::error!(%task_id, %error, "One of the main tasks failed");
                    Err(error)?;
                }
                Err(TaskError::Panic) => {
                    tracing::error!(%task_id, "One of the main tasks panicked");
                    Err(ProxyRuntimeError::TaskPanic(task_id))?;
                }
            },

            (_, TaskUpdate::Message(msg)) => self.handle(msg).await?,
        }

        Ok(())
    }

    /// Routes most messages from the agent to the correct background task.
    ///
    /// Some messages are handled here.
    async fn handle_agent_message(
        &mut self,
        message: DaemonMessage,
    ) -> Result<(), ProxyRuntimeError> {
        if self.ping_pong_update_allowed
            && !matches!(message, DaemonMessage::Pong | DaemonMessage::Close(_))
        {
            self.task_txs
                .ping_pong
                .send(PingPongMessage::AgentSentMessage)
                .await;
            self.ping_pong_update_allowed = false;
        }

        match message {
            DaemonMessage::Pong => {
                self.task_txs
                    .ping_pong
                    .send(PingPongMessage::AgentSentPong)
                    .await
            }
            DaemonMessage::OperatorPing(id) => {
                self.agent_tx.send(ClientMessage::OperatorPong(id)).await
            }
            DaemonMessage::Close(reason) => Err(ProxyRuntimeError::AgentFailed(reason))?,
            DaemonMessage::TcpOutgoing(msg) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::AgentStream(msg))
                    .await
            }
            DaemonMessage::UdpOutgoing(msg) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::AgentDatagrams(msg))
                    .await
            }
            DaemonMessage::File(msg) => {
                self.task_txs
                    .files
                    .send(FilesProxyMessage::FileRes(msg))
                    .await
            }
            DaemonMessage::GetAddrInfoResponse(msg) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::AddrInfoRes(msg))
                    .await
            }
            DaemonMessage::Tcp(msg) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::AgentMirror(msg))
                    .await
            }
            DaemonMessage::TcpSteal(msg) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::AgentSteal(msg))
                    .await
            }
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                let previous = self.protocol_version.replace(protocol_version.clone());
                if previous.is_none() {
                    // We can now process layers' requests.
                    self.background_tasks
                        .resume_messages(MainTaskId::LayerInitializer);
                }

                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    self.agent_tx.send(ClientMessage::ReadyForLogs).await;
                }

                self.task_txs
                    .files
                    .send(FilesProxyMessage::ProtocolVersion(protocol_version.clone()))
                    .await;

                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::ProtocolVersion(
                        protocol_version.clone(),
                    ))
                    .await;

                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::AgentProtocolVersion(
                        protocol_version.clone(),
                    ))
                    .await;

                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::AgentProtocolVersion(protocol_version))
                    .await;
            }
            DaemonMessage::LogMessage(log) => match log.level {
                LogLevel::Error => tracing::error!(
                    message = log.message,
                    "Received a log message from the agent"
                ),
                LogLevel::Warn => tracing::warn!(
                    message = log.message,
                    "Received a log message from the agent"
                ),
                LogLevel::Info => tracing::info!(
                    message = log.message,
                    "Received a log message from the agent"
                ),
            },
            DaemonMessage::GetEnvVarsResponse(res) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::GetEnvRes(res.map(Into::into)))
                    .await
            }
            message @ DaemonMessage::PauseTarget(_)
            | message @ DaemonMessage::Vpn(_)
            | message @ DaemonMessage::ReverseDnsLookup(_) => {
                Err(ProxyRuntimeError::UnexpectedAgentMessage(
                    UnexpectedAgentMessage(message.into()),
                ))?;
            }
        }

        Ok(())
    }

    /// Routes a message from the layer to the correct background task.
    async fn handle_layer_message(&mut self, message: FromLayer) -> Result<(), ProxyRuntimeError> {
        let FromLayer {
            message_id,
            layer_id,
            message,
        } = message;

        match message {
            LayerToProxyMessage::File(req) => {
                self.task_txs
                    .files
                    .send(FilesProxyMessage::FileReq(message_id, layer_id, req))
                    .await;
            }
            LayerToProxyMessage::GetAddrInfo(req) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::AddrInfoReq(message_id, layer_id, req))
                    .await
            }
            LayerToProxyMessage::Outgoing(req) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::Layer(req, message_id, layer_id))
                    .await
            }
            LayerToProxyMessage::Incoming(req) => {
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::LayerRequest(
                        message_id, layer_id, req,
                    ))
                    .await
            }
            LayerToProxyMessage::GetEnv(req) => {
                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::GetEnvReq(message_id, layer_id, req))
                    .await
            }
            other => Err(ProxyRuntimeError::UnexpectedLayerMessage(other))?,
        }

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_connection_refresh(
        &mut self,
        kind: ConnectionRefresh,
    ) -> Result<(), ProxyRuntimeError> {
        self.task_txs
            .ping_pong
            .send(PingPongMessage::ConnectionRefresh(
                kind.clone_with_another_handle(),
            ))
            .await;

        self.task_txs
            .files
            .send(FilesProxyMessage::ConnectionRefresh(
                kind.clone_with_another_handle(),
            ))
            .await;

        self.task_txs
            .incoming
            .send(IncomingProxyMessage::ConnectionRefresh(
                kind.clone_with_another_handle(),
            ))
            .await;

        self.task_txs
            .outgoing
            .send(OutgoingProxyMessage::ConnectionRefresh(
                kind.clone_with_another_handle(),
            ))
            .await;

        self.task_txs
            .simple
            .send(SimpleProxyMessage::ConnectionRefresh(
                kind.clone_with_another_handle(),
            ))
            .await;

        match kind {
            ConnectionRefresh::Start => {
                // Initialise default reconnect message queue
                self.reconnect_task_queue.get_or_insert_default();
            }
            ConnectionRefresh::End(new_agent_tx) => {
                let task_queue = self.reconnect_task_queue.take().unwrap_or_else(|| {
                    tracing::error!("Unexpected state: agent reconnect finished without correctly initializing a reconnect");
                    panic!("agent reconnect finished without correctly initializing a reconnect");
                });

                self.agent_tx = new_agent_tx;

                self.agent_tx
                    .send(ClientMessage::SwitchProtocolVersion(
                        self.protocol_version
                            .as_ref()
                            .unwrap_or(&mirrord_protocol::VERSION)
                            .clone(),
                    ))
                    .await;

                Box::pin(async {
                    for msg in task_queue {
                        tracing::debug!(?msg, "dequeueing message for reconnect");

                        self.handle(msg).await?;
                    }

                    Ok::<(), ProxyRuntimeError>(())
                })
                .await?;
            }
            ConnectionRefresh::Request => {
                self.task_txs
                    .agent
                    .send(AgentConnectionMessage::RequestReconnect)
                    .await;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{net::SocketAddr, path::PathBuf, time::Duration};

    use hyper::{HeaderMap, Method, StatusCode, Uri, Version};
    use mirrord_analytics::NullReporter;
    use mirrord_config::{
        LayerFileConfig, config::MirrordConfig, experimental::ExperimentalFileConfig,
    };
    use mirrord_intproxy_protocol::{
        IncomingRequest, LayerToProxyMessage, LocalMessage, NetProtocol, NewSessionRequest,
        OutgoingConnectRequest, OutgoingRequest, OutgoingResponse, PortSubscribe, PortSubscription,
        ProcessInfo, ProxyToLayerMessage,
        codec::{AsyncDecoder, AsyncEncoder},
    };
    use mirrord_protocol::{
        ClientMessage, DaemonMessage, ErrorKindInternal, FileRequest, FileResponse, RemoteIOError,
        ResponseError, VERSION,
        dns::{AddressFamily, GetAddrInfoRequestV2, GetAddrInfoResponse, SockType},
        file::{OpenFileRequest, StatFsRequestV2},
        outgoing::{LayerConnectV2, SocketAddress, tcp::LayerTcpOutgoing},
        tcp::{
            ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, DaemonTcp,
            HttpRequestMetadata, HttpResponse, IncomingTrafficTransportType, InternalHttpBodyFrame,
            InternalHttpBodyNew, InternalHttpRequest, InternalHttpResponse, LayerTcpSteal,
            StealType,
        },
    };
    use mirrord_protocol_io::{Client, Connection, ConnectionOutput};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{
            TcpListener, TcpStream,
            tcp::{OwnedReadHalf, OwnedWriteHalf},
        },
        sync::mpsc,
    };

    use crate::{
        IntProxy,
        agent_conn::{
            AgentConnectInfo, AgentConnectInfoDiscriminants, AgentConnection, ReconnectFlow,
        },
    };

    /// Verifies that [`IntProxy`] waits with processing layers' requests
    /// until [`mirrord_protocol`] version is negotiated.
    ///
    /// # TODO
    ///
    /// This test does a short sleep.
    /// Once intproxy exposes its state in some way, we can remove it.
    #[tokio::test]
    async fn intproxy_waits_for_protocol_version() {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (connection, proxy_tx, proxy_rx) = Connection::dummy();

        let agent_conn = AgentConnection {
            connection,
            reconnect: ReconnectFlow::Break(AgentConnectInfoDiscriminants::DirectKubernetes),
        };
        let proxy = IntProxy::new_with_connection(
            agent_conn,
            listener,
            4096,
            Default::default(),
            Duration::from_secs(60),
            &ExperimentalFileConfig::default()
                .generate_config(&mut Default::default())
                .unwrap(),
        );
        let proxy_handle = tokio::spawn(proxy.run(Duration::from_secs(60), Duration::ZERO));

        match proxy_rx.next().await.unwrap() {
            ClientMessage::SwitchProtocolVersion(version) => {
                assert_eq!(version, *mirrord_protocol::VERSION)
            }
            other => panic!("unexpected client message from the proxy: {other:?}"),
        }

        let conn = TcpStream::connect(proxy_addr).await.unwrap();
        let mut codec = mirrord_intproxy_protocol::codec::make_async_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(conn);
        codec
            .0
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::NewSession(NewSessionRequest {
                    process_info: ProcessInfo {
                        pid: 1337,
                        parent_pid: 1336,
                        name: "hello there".into(),
                        cmdline: vec!["hello there".into()],
                        loaded: true,
                    },
                    parent_layer: None,
                }),
            })
            .await
            .unwrap();
        codec.0.flush().await.unwrap();
        match codec.1.receive().await.unwrap().unwrap() {
            LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::NewSession(..),
            } => {}
            other => panic!("unexpected local message from the proxy: {other:?}"),
        }

        codec
            .0
            .send(&LocalMessage {
                message_id: 1,
                inner: LayerToProxyMessage::File(FileRequest::StatFsV2(StatFsRequestV2 {
                    path: PathBuf::from("/some/path"),
                })),
            })
            .await
            .unwrap();
        codec.0.flush().await.unwrap();

        // To make sure that the proxy has a chance to do progress.
        // If the proxy was not waiting for agent protocol version,
        // it would surely respond to our previous request here.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match proxy_rx.next().await.unwrap() {
                    ClientMessage::Ping => {
                        proxy_tx.send(DaemonMessage::Pong).await.unwrap();
                    }
                    other => panic!("unexpected client message from the proxy: {other:?}"),
                }
            }
        })
        .await
        .unwrap_err();

        proxy_tx
            .send(DaemonMessage::SwitchProtocolVersionResponse(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
            .unwrap();

        loop {
            match proxy_rx.next().await.unwrap() {
                ClientMessage::Ping => {
                    proxy_tx.send(DaemonMessage::Pong).await.unwrap();
                }
                ClientMessage::ReadyForLogs => {}
                ClientMessage::FileRequest(FileRequest::StatFsV2(StatFsRequestV2 { path })) => {
                    assert_eq!(path, PathBuf::from("/some/path"));
                    break;
                }
                other => panic!("unexpected client message from the proxy: {other:?}"),
            }
        }

        std::mem::drop(codec);

        proxy_handle.await.unwrap().unwrap();
    }
    /// Verifies that [`IntProxy`] goes in failover state when a runtime error happens
    #[tokio::test]
    async fn switch_to_failover() {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (connection, proxy_tx, _proxy_rx) = Connection::dummy();

        let agent_conn = AgentConnection {
            connection,
            reconnect: ReconnectFlow::Break(AgentConnectInfoDiscriminants::DirectKubernetes),
        };

        let proxy = IntProxy::new_with_connection(
            agent_conn,
            listener,
            4096,
            Default::default(),
            Duration::from_secs(60),
            &ExperimentalFileConfig::default()
                .generate_config(&mut Default::default())
                .unwrap(),
        );
        let proxy_handle = tokio::spawn(proxy.run(Duration::from_secs(60), Duration::ZERO));

        let conn = TcpStream::connect(proxy_addr).await.unwrap();
        let (mut encoder, mut decoder) = mirrord_intproxy_protocol::codec::make_async_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(conn);

        encoder
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::NewSession(NewSessionRequest {
                    process_info: ProcessInfo {
                        pid: 1337,
                        parent_pid: 1336,
                        name: "hello there".into(),
                        cmdline: vec!["hello there".into()],
                        loaded: true,
                    },
                    parent_layer: None,
                }),
            })
            .await
            .unwrap();
        encoder.flush().await.unwrap();
        match decoder.receive().await.unwrap().unwrap() {
            LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::NewSession(..),
            } => {}
            other => panic!("unexpected local message from the proxy: {other:?}"),
        }

        proxy_tx
            .send(DaemonMessage::SwitchProtocolVersionResponse(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
            .unwrap();

        proxy_tx
            .send(DaemonMessage::Close("no reason".to_string()))
            .await
            .unwrap();

        encoder
            .send(&LocalMessage {
                message_id: 1,
                inner: LayerToProxyMessage::File(FileRequest::StatFsV2(StatFsRequestV2 {
                    path: PathBuf::from("/some/path"),
                })),
            })
            .await
            .unwrap();
        encoder.flush().await.unwrap();

        match decoder.receive().await.unwrap().unwrap() {
            LocalMessage {
                message_id: 1,
                inner: ProxyToLayerMessage::ProxyFailed(..),
            } => {}
            other => panic!("unexpected local message from the proxy: {other:?}"),
        }

        std::mem::drop((encoder, decoder));

        proxy_handle.await.unwrap().unwrap();
    }

    /// Verifies that [`IntProxy`] run method return an error on a startup error
    #[tokio::test]
    async fn startup_fail() {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();

        let (connection, _proxy_tx, _proxy_rx) = Connection::dummy();

        let agent_conn = AgentConnection {
            connection,
            reconnect: ReconnectFlow::Break(AgentConnectInfoDiscriminants::DirectKubernetes),
        };

        let proxy = IntProxy::new_with_connection(
            agent_conn,
            listener,
            4096,
            Default::default(),
            Duration::from_secs(60),
            &ExperimentalFileConfig::default()
                .generate_config(&mut Default::default())
                .unwrap(),
        );
        tokio::time::timeout(
            Duration::from_millis(200),
            proxy.run(Duration::from_millis(100), Duration::ZERO),
        )
        .await
        .unwrap()
        .unwrap_err();
    }

    struct ReconnectTestSetup {
        conn_rx: mpsc::Receiver<(mpsc::Sender<DaemonMessage>, ConnectionOutput<Client>)>,
        from_layer: AsyncEncoder<LocalMessage<LayerToProxyMessage>, OwnedWriteHalf>,
        to_layer: AsyncDecoder<LocalMessage<ProxyToLayerMessage>, OwnedReadHalf>,
    }

    async fn setup_reconnect_test() -> ReconnectTestSetup {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();

        let (conn_tx, conn_rx) = mpsc::channel(1);

        let config = LayerFileConfig::default()
            .generate_config(&mut Default::default())
            .unwrap();

        let agent_conn = AgentConnection::new(
            &config,
            AgentConnectInfo::Dummy(conn_tx),
            &mut NullReporter::default(),
        )
        .await
        .unwrap();

        let conn = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();

        let (mut from_layer, mut to_layer) = mirrord_intproxy_protocol::codec::make_async_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(conn);

        from_layer
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::NewSession(NewSessionRequest {
                    process_info: ProcessInfo {
                        pid: 1337,
                        parent_pid: 1336,
                        name: "hello there".into(),
                        cmdline: vec!["hello there".into()],
                        loaded: true,
                    },
                    parent_layer: None,
                }),
            })
            .await
            .unwrap();
        from_layer.flush().await.unwrap();

        let proxy = IntProxy::new_with_connection(
            agent_conn,
            listener,
            4096,
            Default::default(),
            Duration::from_secs(60),
            &ExperimentalFileConfig::default()
                .generate_config(&mut Default::default())
                .unwrap(),
        );
        tokio::spawn(proxy.run(Duration::from_millis(100), Duration::ZERO));

        match to_layer.receive().await.unwrap().unwrap() {
            LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::NewSession(..),
            } => {}
            other => panic!("unexpected local message from the proxy: {other:?}"),
        }

        ReconnectTestSetup {
            from_layer,
            to_layer,
            conn_rx,
        }
    }

    async fn next_proxy_msg(
        to_proxy: &mpsc::Sender<DaemonMessage>,
        from_proxy: &ConnectionOutput<Client>,
    ) -> ClientMessage {
        loop {
            match from_proxy.next().await.unwrap() {
                ClientMessage::Ping => to_proxy.send(DaemonMessage::Pong).await.unwrap(),
                ClientMessage::ReadyForLogs => (),
                other => return other,
            }
        }
    }

    async fn switch_protocol_version(
        to_proxy: &mpsc::Sender<DaemonMessage>,
        from_proxy: &ConnectionOutput<Client>,
    ) {
        assert_eq!(
            next_proxy_msg(to_proxy, from_proxy).await,
            ClientMessage::SwitchProtocolVersion(VERSION.clone()),
        );

        to_proxy
            .send(DaemonMessage::SwitchProtocolVersionResponse(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
            .unwrap();
    }

    /// Verifies that [`IntProxy`] reconnects and restores state (port subscriptions) correctly
    #[tokio::test]
    #[rstest::rstest]
    #[timeout(Duration::from_secs(5))]
    async fn reconnect_restore_subscriptions() {
        let ReconnectTestSetup {
            mut conn_rx,
            mut from_layer,
            mut to_layer,
        } = setup_reconnect_test().await;

        let (mut to_proxy, mut from_proxy) = conn_rx.recv().await.unwrap();

        // Subscribe to a port so we can later confirm that it
        // restores the subscription after the reconnect.
        from_layer
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::Incoming(IncomingRequest::PortSubscribe(
                    PortSubscribe {
                        listening_on: "0.0.0.0:42069".parse().unwrap(),
                        subscription: PortSubscription::Steal(StealType::All(42069)),
                    },
                )),
            })
            .await
            .unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert_eq!(
            next_proxy_msg(&to_proxy, &from_proxy).await,
            ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(42069)))
        );

        to_proxy
            .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
                42069,
            ))))
            .await
            .unwrap();

        // Run multiple times for good measure
        for _ in 0..3 {
            // Simulate dropping the connection
            drop(to_proxy);

            // The intproxy (AgentConnection to be exact) should now try to reconnect.
            // We'll receive the new dummy connection from conn_rx.

            (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

            switch_protocol_version(&to_proxy, &from_proxy).await;

            assert_eq!(
                next_proxy_msg(&to_proxy, &from_proxy).await,
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(42069)))
            );

            to_proxy
                .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
                    42069,
                ))))
                .await
                .unwrap();

            assert!(matches!(
                to_layer.receive().await,
                Ok(Some(LocalMessage {
                    message_id: 0,
                    inner: ProxyToLayerMessage::Incoming(
                        mirrord_intproxy_protocol::IncomingResponse::PortSubscribe(Ok(()))
                    )
                }))
            ));
        }
    }

    /// Verifies that [`IntProxy`] reconnects correctly when a pong is no received.
    #[tokio::test]
    #[rstest::rstest]
    #[timeout(Duration::from_secs(5))]
    async fn reconnect_on_lost_ping(#[values(true, false)] drop_explicitly: bool) {
        let ReconnectTestSetup {
            mut conn_rx,
            // Keep the connection so intproxy doesn't exit
            from_layer: _from_layer,
            to_layer: _,
        } = setup_reconnect_test().await;

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert_eq!(from_proxy.next().await, Some(ClientMessage::ReadyForLogs));
        assert_eq!(from_proxy.next().await, Some(ClientMessage::Ping));

        // Don't respond to pings.
        if drop_explicitly {
            drop(to_proxy);
        }

        // We should get a reconnect.

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert_eq!(from_proxy.next().await, Some(ClientMessage::ReadyForLogs));
        assert_eq!(from_proxy.next().await, Some(ClientMessage::Ping));
    }

    /// Verifies that [`IntProxy`] reconnects correctly while waiting for a fileops response.
    #[tokio::test]
    #[rstest::rstest]
    #[timeout(Duration::from_secs(5))]
    async fn reconnect_during_fileop() {
        let ReconnectTestSetup {
            mut conn_rx,
            // Keep the connection so intproxy doesn't exit
            mut from_layer,
            mut to_layer,
        } = setup_reconnect_test().await;

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        let file_request = FileRequest::Open(OpenFileRequest {
            path: "/some/file".into(),
            open_options: Default::default(),
        });

        from_layer
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::File(file_request.clone()),
            })
            .await
            .unwrap();

        assert_eq!(
            next_proxy_msg(&to_proxy, &from_proxy).await,
            ClientMessage::FileRequest(file_request)
        );

        drop(to_proxy);

        // We should get a reconnect.

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();
        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert!(matches!(
            to_layer.receive().await,
            Ok(Some(LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::File(FileResponse::Open(Err(ResponseError::RemoteIO(
                    RemoteIOError {
                        raw_os_error: None,
                        kind: ErrorKindInternal::Unknown(error_msg)
                    }
                ))))
            })) if error_msg == "connection with mirrord-agent was lost"
        ));
    }

    /// Verifies that [`IntProxy`] reconnects correctly while waiting for a response to a
    /// [`ClientMessage::TcpOutgoing`].
    #[tokio::test]
    #[rstest::rstest]
    #[timeout(Duration::from_secs(5))]
    async fn reconnect_during_outgoing() {
        let ReconnectTestSetup {
            mut conn_rx,
            // Keep the connection so intproxy doesn't exit
            mut from_layer,
            mut to_layer,
        } = setup_reconnect_test().await;

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        let socket_addr = SocketAddress::Ip("8.0.0.85:69".parse().unwrap());

        from_layer
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::Outgoing(OutgoingRequest::Connect(
                    OutgoingConnectRequest {
                        remote_address: socket_addr.clone(),
                        protocol: NetProtocol::Stream,
                    },
                )),
            })
            .await
            .unwrap();

        assert!(matches!(
            next_proxy_msg(&to_proxy, &from_proxy).await,
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
                remote_address,
                ..
            })) if remote_address == socket_addr
        ));

        drop(to_proxy);

        // We should get a reconnect.

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();
        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert!(matches!(
            to_layer.receive().await,
            Ok(Some(LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                    ResponseError::RemoteIO(RemoteIOError {
                        raw_os_error: None,
                        kind: ErrorKindInternal::Unknown(error_msg)
                    })
                )))
            })) if error_msg == "connection with mirrord-agent was lost"
        ));
    }

    /// Verifies that [`IntProxy`] reconnects correctly while waiting for dns response
    #[tokio::test]
    #[rstest::rstest]
    #[timeout(Duration::from_secs(5))]
    async fn reconnect_during_dns() {
        let ReconnectTestSetup {
            mut conn_rx,
            // Keep the connection so intproxy doesn't exit
            mut from_layer,
            mut to_layer,
        } = setup_reconnect_test().await;

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        let request = GetAddrInfoRequestV2 {
            node: "hello".into(),
            service_port: 4000,
            family: AddressFamily::Ipv4Only,
            socktype: SockType::Stream,
            flags: 0,
            protocol: 0,
        };

        from_layer
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::GetAddrInfo(request.clone()),
            })
            .await
            .unwrap();

        assert_eq!(
            next_proxy_msg(&to_proxy, &from_proxy).await,
            ClientMessage::GetAddrInfoRequestV2(request)
        );

        drop(to_proxy);

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();
        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert!(matches!(
            to_layer.receive().await,
            Ok(Some(LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::GetAddrInfo(GetAddrInfoResponse(Err(
                    ResponseError::RemoteIO(RemoteIOError {
                        raw_os_error: None,
                        kind: ErrorKindInternal::Unknown(error_msg)
                    })
                )))
            })) if error_msg == "connection with mirrord-agent was lost"
        ));
    }

    /// Verifies that [`IntProxy`] reconnects correctly while it was serving a stolen request
    #[tokio::test]
    #[rstest::rstest]
    #[timeout(Duration::from_secs(5))]
    async fn reconnect_during_http(#[values(true, false)] drop_during_response: bool) {
        let ReconnectTestSetup {
            mut conn_rx,
            // Keep the connection so intproxy doesn't exit
            mut from_layer,
            mut to_layer,
        } = setup_reconnect_test().await;

        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();

        switch_protocol_version(&to_proxy, &from_proxy).await;

        from_layer
            .send(&LocalMessage {
                message_id: 0,
                inner: LayerToProxyMessage::Incoming(IncomingRequest::PortSubscribe(
                    PortSubscribe {
                        listening_on: addr,
                        subscription: PortSubscription::Steal(StealType::All(80)),
                    },
                )),
            })
            .await
            .unwrap();

        assert_eq!(
            next_proxy_msg(&to_proxy, &from_proxy).await,
            ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(80)))
        );

        to_proxy
            .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(80))))
            .await
            .unwrap();

        assert!(matches!(
            to_layer.receive().await,
            Ok(Some(LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::Incoming(
                    mirrord_intproxy_protocol::IncomingResponse::PortSubscribe(Ok(()))
                )
            }))
        ));

        let mut headers = HeaderMap::new();
        headers.insert("content-length", "hello".len().into());

        to_proxy
            .send(DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                    connection_id: 0,
                    request_id: 0,
                    request: InternalHttpRequest {
                        method: Method::GET,
                        uri: Uri::from_static("/test/"),
                        headers: headers.clone(),
                        version: Version::HTTP_10,
                        body: InternalHttpBodyNew {
                            frames: vec![InternalHttpBodyFrame::Data("he".into())],
                            is_last: false,
                        },
                    },
                    metadata: HttpRequestMetadata::V1 {
                        source: "1.2.3.4:58239".parse().unwrap(),
                        destination: "1.1.1.1:80".parse().unwrap(),
                    },
                    transport: IncomingTrafficTransportType::Tcp,
                }),
            )))
            .await
            .unwrap();

        let (mut conn, _) = server.accept().await.unwrap();

        const EXPECTED_REQUEST: &[u8] = b"GET /test/ HTTP/1.0\r\ncontent-length: 5\r\n\r\nhello";

        if drop_during_response {
            to_proxy
                .send(DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(
                    ChunkedRequest::Body(ChunkedRequestBodyV1 {
                        frames: vec![InternalHttpBodyFrame::Data("llo".into())],
                        is_last: true,
                        connection_id: 0,
                        request_id: 0,
                    }),
                )))
                .await
                .unwrap();

            let mut buf = [0u8; EXPECTED_REQUEST.len()];
            conn.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, EXPECTED_REQUEST);

            conn.write_all(b"HTTP/1.0 200 OK\r\ncontent-length: 5\r\n\r\nwo")
                .await
                .unwrap();

            assert_eq!(
                next_proxy_msg(&to_proxy, &from_proxy).await,
                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                    mirrord_protocol::tcp::ChunkedResponse::Start(HttpResponse {
                        port: 80,
                        connection_id: 0,
                        request_id: 0,
                        internal_response: InternalHttpResponse {
                            status: StatusCode::OK,
                            version: Version::HTTP_10,
                            headers: headers.clone(),
                            body: vec![InternalHttpBodyFrame::Data("wo".into())]
                        }
                    })
                ))
            );
        }

        drop(to_proxy);

        // Assert intproxy closes the connection.
        //
        // `read_to_end` reads until EOF, which is the *graceful*
        // termination by of TCP connection by peer (a.k.a intproxy).
        // Note that the connection may terminate before we're able to
        // read the entire request (and often before we're able to
        // read a single byte), so we're only checking whatever we
        // *do* manage to receive. In any case all we care about here
        // is whether the intproxy closes the connection gracefully or
        // not.

        let mut buf = vec![];
        let read_bytes = conn.read_to_end(&mut buf).await.unwrap();
        if drop_during_response {
            // We've already responded, intproxy is (was) waiting for
            // the rest of the response so we have nothing to receive.
            // We should just wait for it to close the connection
            // immediately.
            assert_eq!(read_bytes, 0);
        } else {
            assert_eq!(
                buf.get(..read_bytes).unwrap(),
                EXPECTED_REQUEST.get(..read_bytes).unwrap()
            );
        }

        let (to_proxy, from_proxy) = conn_rx.recv().await.unwrap();
        switch_protocol_version(&to_proxy, &from_proxy).await;

        assert_eq!(
            next_proxy_msg(&to_proxy, &from_proxy).await,
            ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(80)))
        );

        to_proxy
            .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(80))))
            .await
            .unwrap();

        assert!(matches!(
            to_layer.receive().await,
            Ok(Some(LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::Incoming(
                    mirrord_intproxy_protocol::IncomingResponse::PortSubscribe(Ok(()))
                )
            }))
        ));
    }
}
