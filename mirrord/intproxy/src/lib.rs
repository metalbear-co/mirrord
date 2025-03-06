#![feature(map_try_insert, let_chains)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use background_tasks::{BackgroundTasks, TaskSender, TaskUpdate};
use error::UnexpectedAgentMessage;
use layer_conn::LayerConnection;
use layer_initializer::LayerInitializer;
use main_tasks::{FromLayer, LayerForked, MainTaskId, ProxyMessage, ToLayer};
use mirrord_config::feature::network::incoming::https_delivery::LocalHttpsDelivery;
use mirrord_intproxy_protocol::{LayerId, LayerToProxyMessage, LocalMessage};
use mirrord_protocol::{ClientMessage, DaemonMessage, LogLevel, CLIENT_READY_FOR_LOGS};
use ping_pong::{PingPong, PingPongMessage};
use proxies::{
    files::{FilesProxy, FilesProxyMessage},
    incoming::{IncomingProxy, IncomingProxyMessage},
    outgoing::{OutgoingProxy, OutgoingProxyMessage},
    simple::{SimpleProxy, SimpleProxyMessage},
};
use semver::Version;
use tokio::{net::TcpListener, time};

use crate::{
    agent_conn::AgentConnection,
    background_tasks::{RestartableBackgroundTaskWrapper, TaskError},
    error::IntProxyError,
    main_tasks::{ConnectionRefresh, LayerClosed},
};

pub mod agent_conn;
pub mod background_tasks;
pub mod error;
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
    outgoing: TaskSender<OutgoingProxy>,
    incoming: TaskSender<IncomingProxy>,
    ping_pong: TaskSender<PingPong>,
    files: TaskSender<FilesProxy>,
}

/// This struct contains logic for proxying between multiple layer instances and one agent.
/// It maintains a singe agent connection.
///
/// Utilizes multiple [`BackgroundTask`](background_tasks::BackgroundTask)s to split logic of
/// different mirrod features (e.g. file operations and incoming traffic).
pub struct IntProxy {
    any_connection_accepted: bool,
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
    task_txs: TaskTxs,

    /// [`mirrord_protocol`] version negotiated with the agent.
    protocol_version: Option<Version>,

    /// Temporary message queue for any [`ProxyMessage`] from layer or to agent that are sent
    /// during reconnection state.
    reconnect_task_queue: Option<VecDeque<ProxyMessage>>,
}

impl IntProxy {
    /// Size of channels used to communicate with main tasks (see [`MainTaskId`]).
    const CHANNEL_SIZE: usize = 512;
    /// How long can the agent connection remain silent.
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    /// Creates a new [`IntProxy`] using existing [`AgentConnection`].
    /// The returned instance will accept connections from the layers using the given
    /// [`TcpListener`].
    pub fn new_with_connection(
        agent_conn: AgentConnection,
        listener: TcpListener,
        file_buffer_size: u64,
        idle_local_http_connection_timeout: Duration,
        https_delivery: LocalHttpsDelivery,
    ) -> Self {
        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
            Default::default();

        let agent = background_tasks.register_restartable(
            agent_conn,
            MainTaskId::AgentConnection,
            Self::CHANNEL_SIZE,
        );
        let layer_initializer = background_tasks.register(
            LayerInitializer::new(listener),
            MainTaskId::LayerInitializer,
            Self::CHANNEL_SIZE,
        );
        // We need to negotiate mirrord-protocol version
        // before we can process layers' requests.
        //
        // If we don't do this, we risk responding with `NotImplemented`
        // to requests that have a requirement on the mirrord-protocol version.
        background_tasks.suspend_messages(MainTaskId::LayerInitializer);
        let ping_pong = background_tasks.register(
            PingPong::new(Self::PING_INTERVAL),
            MainTaskId::PingPong,
            Self::CHANNEL_SIZE,
        );
        let simple = background_tasks.register(
            SimpleProxy::default(),
            MainTaskId::SimpleProxy,
            Self::CHANNEL_SIZE,
        );
        let outgoing = background_tasks.register(
            OutgoingProxy::default(),
            MainTaskId::OutgoingProxy,
            Self::CHANNEL_SIZE,
        );
        let incoming = background_tasks.register(
            IncomingProxy::new(idle_local_http_connection_timeout, https_delivery),
            MainTaskId::IncomingProxy,
            Self::CHANNEL_SIZE,
        );
        let files = background_tasks.register(
            FilesProxy::new(file_buffer_size),
            MainTaskId::FilesProxy,
            Self::CHANNEL_SIZE,
        );

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
            protocol_version: None,
            reconnect_task_queue: Default::default(),
        }
    }

    /// Runs main event loop of this proxy.
    /// Expects to accept the first layer connection within the given `first_timeout`.
    /// Exits after `idle_timeout` when there are no more layer connections.
    pub async fn run(
        mut self,
        first_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<(), IntProxyError> {
        self.task_txs
            .agent
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        loop {
            tokio::select! {
                Some((task_id, task_update)) = self.background_tasks.next() => {
                    tracing::trace!(
                        %task_id,
                        ?task_update,
                        "Received a task update",
                    );

                    self.handle_task_update(task_id, task_update).await?;
                }

                _ = time::sleep(first_timeout), if !self.any_connection_accepted => {
                    return Err(IntProxyError::ConnectionAcceptTimeout);
                },

                _ = time::sleep(idle_timeout), if self.any_connection_accepted && self.task_txs.layers.is_empty() => {
                    tracing::info!("Reached the idle timeout with no active layer connections");
                    break;
                },
            }
        }

        std::mem::drop(self.task_txs);

        tracing::info!("Collecting background task results before exiting");
        let results = self.background_tasks.results().await;

        for (task_id, result) in results {
            tracing::trace!(
                %task_id,
                ?result,
                "Collected a background task result",
            );
        }

        Ok(())
    }

    /// Routes a [`ProxyMessage`] to the correct background task.
    /// [`ProxyMessage::NewLayer`] is handled here, as an exception.
    async fn handle(&mut self, msg: ProxyMessage) -> Result<(), IntProxyError> {
        match msg {
            ProxyMessage::NewLayer(_) | ProxyMessage::FromLayer(_) | ProxyMessage::ToAgent(_)
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
                }
            }
            ProxyMessage::FromAgent(msg) => self.handle_agent_message(msg).await?,
            ProxyMessage::FromLayer(msg) => self.handle_layer_message(msg).await?,
            ProxyMessage::ToAgent(msg) => self.task_txs.agent.send(msg).await,
            ProxyMessage::ToLayer(msg) => {
                let ToLayer {
                    message,
                    message_id,
                    layer_id,
                } = msg;

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
        update: TaskUpdate<ProxyMessage, IntProxyError>,
    ) -> Result<(), IntProxyError> {
        match (task_id, update) {
            (MainTaskId::LayerConnection(LayerId(id)), TaskUpdate::Finished(Ok(()))) => {
                tracing::trace!(layer_id = id, "Layer connection closed");

                let msg = LayerClosed { id: LayerId(id) };

                self.task_txs
                    .files
                    .send(FilesProxyMessage::LayerClosed(msg))
                    .await;
                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::LayerClosed(msg))
                    .await;

                self.task_txs.layers.remove(&LayerId(id));
            }

            (task_id, TaskUpdate::Finished(res)) => match res {
                Ok(()) => {
                    tracing::error!(%task_id, "One of the main tasks finished unexpectedly");
                    return Err(IntProxyError::TaskExit(task_id));
                }
                Err(TaskError::Error(error)) => {
                    tracing::error!(%task_id, %error, "One of the main tasks failed");
                    return Err(error);
                }
                Err(TaskError::Panic) => {
                    tracing::error!(%task_id, "One of the main tasks panicked");
                    return Err(IntProxyError::TaskPanic(task_id));
                }
            },

            (_, TaskUpdate::Message(msg)) => self.handle(msg).await?,
        }

        Ok(())
    }

    /// Routes most messages from the agent to the correct background task.
    /// Some messages are handled here.
    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<(), IntProxyError> {
        match message {
            DaemonMessage::Pong => {
                self.task_txs
                    .ping_pong
                    .send(PingPongMessage::AgentSentPong)
                    .await
            }
            DaemonMessage::Close(reason) => return Err(IntProxyError::AgentFailed(reason)),
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
                    self.task_txs.agent.send(ClientMessage::ReadyForLogs).await;
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
                    .send(IncomingProxyMessage::AgentProtocolVersion(protocol_version))
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
                    .send(SimpleProxyMessage::GetEnvRes(res))
                    .await
            }
            other => {
                return Err(IntProxyError::UnexpectedAgentMessage(
                    UnexpectedAgentMessage(other),
                ));
            }
        }

        Ok(())
    }

    /// Routes a message from the layer to the correct background task.
    async fn handle_layer_message(&self, message: FromLayer) -> Result<(), IntProxyError> {
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
            LayerToProxyMessage::OutgoingConnect(req) => {
                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::LayerConnect(
                        req, message_id, layer_id,
                    ))
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
            other => return Err(IntProxyError::UnexpectedLayerMessage(other)),
        }

        Ok(())
    }

    async fn handle_connection_refresh(
        &mut self,
        kind: ConnectionRefresh,
    ) -> Result<(), IntProxyError> {
        self.task_txs
            .ping_pong
            .send(PingPongMessage::ConnectionRefresh(kind))
            .await;

        match kind {
            ConnectionRefresh::Start => {
                // Initialise default reconnect message queue
                self.reconnect_task_queue.get_or_insert_default();

                self.task_txs
                    .simple
                    .send(SimpleProxyMessage::ConnectionRefresh)
                    .await;

                self.task_txs
                    .outgoing
                    .send(OutgoingProxyMessage::ConnectionRefresh)
                    .await;
            }
            ConnectionRefresh::End => {
                let task_queue = self.reconnect_task_queue.take().unwrap_or_else(|| {
                    tracing::error!("Unexpected state: agent reconnect finished without correctly initializing a reconnect");
                    panic!("agent reconnect finished without correctly initializing a reconnect");
                });

                self.task_txs
                    .agent
                    .send(ClientMessage::SwitchProtocolVersion(
                        self.protocol_version
                            .as_ref()
                            .unwrap_or(&mirrord_protocol::VERSION)
                            .clone(),
                    ))
                    .await;

                self.task_txs
                    .files
                    .send(FilesProxyMessage::ConnectionRefresh)
                    .await;

                self.task_txs
                    .incoming
                    .send(IncomingProxyMessage::ConnectionRefresh)
                    .await;

                Box::pin(async {
                    for msg in task_queue {
                        tracing::debug!(?msg, "dequeueing message for reconnect");

                        self.handle(msg).await?;
                    }

                    Ok::<(), IntProxyError>(())
                })
                .await?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{net::SocketAddr, path::PathBuf, time::Duration};

    use mirrord_intproxy_protocol::{
        LayerToProxyMessage, LocalMessage, NewSessionRequest, ProcessInfo, ProxyToLayerMessage,
    };
    use mirrord_protocol::{file::StatFsRequestV2, ClientMessage, DaemonMessage, FileRequest};
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    use crate::{
        agent_conn::{AgentConnection, ReconnectFlow},
        IntProxy,
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
        let (agent_tx, mut proxy_rx) = mpsc::channel(12);
        let (proxy_tx, agent_rx) = mpsc::channel(12);

        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let agent_conn = AgentConnection {
            agent_rx,
            agent_tx,
            reconnect: ReconnectFlow::Break,
        };
        let proxy = IntProxy::new_with_connection(
            agent_conn,
            listener,
            4096,
            Default::default(),
            Default::default(),
        );
        let proxy_handle = tokio::spawn(proxy.run(Duration::from_secs(60), Duration::ZERO));

        match proxy_rx.recv().await.unwrap() {
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
                inner: LayerToProxyMessage::NewSession(NewSessionRequest::New(ProcessInfo {
                    pid: 1337,
                    name: "hello there".into(),
                    cmdline: vec!["hello there".into()],
                    loaded: true,
                })),
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
                match proxy_rx.recv().await.unwrap() {
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
            match proxy_rx.recv().await.unwrap() {
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
}
