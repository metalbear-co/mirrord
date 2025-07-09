use std::{
    collections::HashMap,
    mem,
    net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6},
    ops::Not,
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use client_connection::AgentTlsConnector;
use dns::{ClientGetAddrInfoRequest, DnsCommand};
use futures::TryFutureExt;
use metrics::{start_metrics, CLIENT_COUNT};
use mirrord_agent_env::envs;
use mirrord_agent_iptables::{
    error::IPTablesError, new_ip6tables, new_iptables, IPTablesWrapper, SafeIpTables,
};
use mirrord_protocol::{ClientMessage, DaemonMessage, GetEnvVarsRequest, LogMessage};
use steal::StealerMessage;
use tokio::{
    net::{TcpListener, TcpStream},
    process::Command,
    select,
    signal::unix::SignalKind,
    sync::mpsc::Sender,
    task::JoinSet,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    cli::{self, Args},
    client_connection::{self, ClientConnection},
    container_handle::ContainerHandle,
    dns::{self, DnsApi},
    env,
    error::{AgentError, AgentResult},
    file::FileManager,
    metrics,
    namespace::NamespaceType,
    outgoing::{TcpOutgoingApi, UdpOutgoingApi},
    runtime::{self, get_container},
    sniffer::{api::TcpSnifferApi, messages::SnifferCommand},
    steal::{self, StealerCommand, TcpStealerApi},
    util::{
        protocol_version::ClientProtocolVersion,
        remote_runtime::{BgTaskRuntime, BgTaskStatus, RemoteRuntime},
        ClientId,
    },
};

mod setup;

/// Size of [`mpsc`](tokio::sync::mpsc) channels connecting [`TcpStealerApi`]s with the background
/// task.
const CHANNEL_SIZE: usize = 1024;

/// [`ExitCode`](std::process::ExitCode) returned from the child agent process
/// when dirty iptables are detected.
pub(crate) const IPTABLES_DIRTY_EXIT_CODE: u8 = 99;

/// Env var that gets checked when a new agent is started.
/// If var is false or not set, the agent starts as an IP table guard which itself starts another
/// agent. The child agent performs normal agent behaviour.
const CHILD_PROCESS_ENV: &str = "MIRRORD_AGENT_CHILD_PROCESS";

/// Keeps track of next client id.
/// Stores common data used when serving client connections.
/// Can be cheaply cloned and passed to per-client background tasks.
#[derive(Clone)]
struct State {
    /// [`ClientId`] for the next client that connects to this agent.
    next_client_id: Arc<AtomicU32>,
    /// Handle to the target container if there is one.
    /// This is optional because it is acceptable not to pass the container runtime and id if not
    /// pausing. When those args are not passed, container is [`None`].
    container: Option<ContainerHandle>,
    env: Arc<HashMap<String, String>>,
    ephemeral: bool,
    /// When present, it is used to secure incoming TCP connections.
    tls_connector: Option<AgentTlsConnector>,
    /// [`tokio::runtime`] that should be used for network operations.
    network_runtime: BgTaskRuntime,
}

impl State {
    /// Return [`Err`] if container runtime operations failed.
    pub async fn new(args: &Args) -> AgentResult<State> {
        let tls_connector = args
            .operator_tls_cert_pem
            .clone()
            .map(AgentTlsConnector::new)
            .transpose()?;

        let mut env: HashMap<String, String> = HashMap::new();

        let (ephemeral, container) = match &args.mode {
            cli::Mode::Targeted {
                container_id,
                container_runtime,
                ..
            } => {
                let container = get_container(container_id.clone(), container_runtime).await?;

                let container_handle = ContainerHandle::new(container).await?;

                env.extend(container_handle.raw_env().clone());

                (false, Some(container_handle))
            }
            cli::Mode::Ephemeral { .. } => {
                let container_handle = ContainerHandle::new(runtime::Container::Ephemeral(
                    runtime::EphemeralContainer {
                        container_id: envs::EPHEMERAL_TARGET_CONTAINER_ID
                            .try_from_env()
                            .ok()
                            .flatten()
                            .unwrap_or_default(),
                    },
                ))
                .await?;

                env.extend(container_handle.raw_env().clone());

                // If we are in an ephemeral container, we use pid 1.
                (true, Some(container_handle))
            }
            cli::Mode::Targetless => (false, None),
        };

        let network_runtime = match container.as_ref().map(ContainerHandle::pid) {
            Some(pid) if ephemeral.not() => BgTaskRuntime::Remote(
                RemoteRuntime::new_in_namespace(pid, NamespaceType::Net).await?,
            ),
            None | Some(..) => BgTaskRuntime::Local,
        };

        let env_pid = match container.as_ref().map(ContainerHandle::pid) {
            Some(pid) => pid.to_string(),
            None => "self".to_string(),
        };
        let environ_path = PathBuf::from("/proc").join(env_pid).join("environ");
        match env::get_proc_environ(environ_path).await {
            Ok(environ) => env.extend(environ.into_iter()),
            Err(err) => {
                error!("Failed to get process environment variables: {err:?}");
            }
        };

        Ok(State {
            next_client_id: Default::default(),
            container,
            env: Arc::new(env),
            ephemeral,
            tls_connector,
            network_runtime,
        })
    }

    /// Return the process ID of the target container if there is one.
    pub fn container_pid(&self) -> Option<u64> {
        self.container.as_ref().map(ContainerHandle::pid)
    }

    pub async fn serve_client_connection(
        self,
        stream: TcpStream,
        tasks: BackgroundTasks,
        cancellation_token: CancellationToken,
    ) -> u32 {
        let client_id = self.next_client_id.fetch_add(1, Ordering::Relaxed);

        let result = ClientConnection::new(stream, client_id, self.tls_connector.clone())
            .map_err(AgentError::from)
            .and_then(|connection| ClientConnectionHandler::new(client_id, connection, tasks, self))
            .and_then(|client| client.start(cancellation_token))
            .await;

        match result {
            Ok(()) => {
                trace!(client_id, "serve_client_connection -> Client disconnected");
            }

            Err(error) => {
                error!(
                    client_id,
                    ?error,
                    "serve_client_connection -> Client disconnected with error",
                );
            }
        }

        client_id
    }

    fn is_with_mesh_exclusion(&self) -> bool {
        self.ephemeral
            && envs::EXCLUDE_FROM_MESH.from_env_or_default()
            && envs::IN_SERVICE_MESH.from_env_or_default()
    }
}
enum BackgroundTask<Command> {
    Running(BgTaskStatus, Sender<Command>),
    Disabled,
}

impl<Command> Clone for BackgroundTask<Command> {
    fn clone(&self) -> Self {
        match self {
            BackgroundTask::Disabled => BackgroundTask::Disabled,
            BackgroundTask::Running(status, sender) => {
                BackgroundTask::Running(status.clone(), sender.clone())
            }
        }
    }
}

/// Handles to background tasks used by [`ClientConnectionHandler`].
#[derive(Clone)]
struct BackgroundTasks {
    sniffer: BackgroundTask<SnifferCommand>,
    stealer: BackgroundTask<StealerCommand>,
    dns: BackgroundTask<DnsCommand>,
}

struct ClientConnectionHandler {
    id: ClientId,
    /// Handles mirrord's file operations, see [`FileManager`].
    file_manager: FileManager,
    connection: ClientConnection,
    /// [`None`] when targetless.
    tcp_sniffer_api: Option<TcpSnifferApi>,
    /// [`None`] when targetless.
    tcp_stealer_api: Option<TcpStealerApi>,
    tcp_outgoing_api: TcpOutgoingApi,
    udp_outgoing_api: UdpOutgoingApi,
    dns_api: DnsApi,
    state: State,
    /// Whether the client has sent us [`ClientMessage::ReadyForLogs`].
    ready_for_logs: bool,
    /// Client's version of [`mirrord_protocol`].
    protocol_version: ClientProtocolVersion,
}

impl Drop for ClientConnectionHandler {
    fn drop(&mut self) {
        CLIENT_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ClientConnectionHandler {
    /// Initializes [`ClientConnectionHandler`].
    #[tracing::instrument(level = Level::TRACE, skip(connection, bg_tasks, state), err)]
    pub async fn new(
        id: ClientId,
        mut connection: ClientConnection,
        bg_tasks: BackgroundTasks,
        state: State,
    ) -> AgentResult<Self> {
        let protocol_version = ClientProtocolVersion::default();

        let pid = state.container_pid();

        let file_manager = FileManager::new(pid.or_else(|| state.ephemeral.then_some(1)));

        let tcp_sniffer_api = Self::create_sniffer_api(id, bg_tasks.sniffer, &mut connection).await;
        let tcp_stealer_api = Self::create_stealer_api(
            id,
            protocol_version.clone(),
            bg_tasks.stealer,
            &mut connection,
        )
        .await?;
        let dns_api = Self::create_dns_api(bg_tasks.dns);

        let tcp_outgoing_api = TcpOutgoingApi::new(&state.network_runtime);
        let udp_outgoing_api = UdpOutgoingApi::new(&state.network_runtime);

        let client_handler = Self {
            id,
            file_manager,
            connection,
            tcp_sniffer_api,
            tcp_stealer_api,
            tcp_outgoing_api,
            udp_outgoing_api,
            dns_api,
            state,
            ready_for_logs: false,
            protocol_version,
        };

        CLIENT_COUNT.fetch_add(1, Ordering::Relaxed);

        Ok(client_handler)
    }

    async fn create_sniffer_api(
        id: ClientId,
        task: BackgroundTask<SnifferCommand>,
        connection: &mut ClientConnection,
    ) -> Option<TcpSnifferApi> {
        if let BackgroundTask::Running(sniffer_status, sniffer_sender) = task {
            match TcpSnifferApi::new(id, sniffer_sender, sniffer_status).await {
                Ok(api) => Some(api),
                Err(e) => {
                    let message = format!(
                        "Failed to create TcpSnifferApi: {e}, this could be due to kernel version."
                    );

                    warn!(message);

                    // Ignore message send error.
                    let _ = connection
                        .send(DaemonMessage::LogMessage(LogMessage::warn(message)))
                        .await;

                    None
                }
            }
        } else {
            None
        }
    }

    async fn create_stealer_api(
        id: ClientId,
        protocol_version: ClientProtocolVersion,
        task: BackgroundTask<StealerCommand>,
        connection: &mut ClientConnection,
    ) -> AgentResult<Option<TcpStealerApi>> {
        if let BackgroundTask::Running(stealer_status, stealer_sender) = task {
            match TcpStealerApi::new(
                id,
                protocol_version,
                stealer_sender,
                stealer_status,
                CHANNEL_SIZE,
            )
            .await
            {
                Ok(api) => Ok(Some(api)),
                Err(e) => {
                    let _ = connection
                        .send(DaemonMessage::Close(format!(
                            "Failed to create TcpStealerApi: {e}."
                        )))
                        .await; // Ignore message send error.

                    Err(e)?
                }
            }
        } else {
            Ok(None)
        }
    }

    fn create_dns_api(task: BackgroundTask<DnsCommand>) -> DnsApi {
        match task {
            BackgroundTask::Running(task_status, task_sender) => {
                DnsApi::new(task_status, task_sender)
            }
            BackgroundTask::Disabled => unreachable!("dns task is never disabled"),
        }
    }

    /// Starts a loop that handles client connection and state.
    ///
    /// Breaks upon receiver/sender drop.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn start(mut self, cancellation_token: CancellationToken) -> AgentResult<()> {
        let error = loop {
            select! {
                message = self.connection.receive() => {
                    let Some(message) = message? else {
                        debug!("Client {} disconnected", self.id);
                        return Ok(());
                    };

                    match self.handle_client_message(message).await {
                        Ok(true) => {},
                        Ok(false) => return Ok(()),
                        Err(e) => {
                            error!("Error handling client message: {e:?}");
                            break e;
                        }
                    }
                },
                // poll the sniffer API only when it's available
                // exit when it stops (means something bad happened if
                // it ran and then stopped)
                message = async {
                    if let Some(ref mut sniffer_api) = self.tcp_sniffer_api {
                        sniffer_api.recv().await
                    } else {
                        unreachable!()
                    }
                }, if self.tcp_sniffer_api.is_some() => match message {
                    Ok((message, Some(log))) if self.ready_for_logs => {
                        self.respond(DaemonMessage::LogMessage(log)).await?;
                        self.respond(DaemonMessage::Tcp(message)).await?;
                    }
                    Ok((message, _)) => {
                        self.respond(DaemonMessage::Tcp(message)).await?;
                    },
                    Err(e) => break e,
                },
                message = async {
                    if let Some(ref mut stealer_api) = self.tcp_stealer_api {
                        stealer_api.recv().await
                    } else {
                        unreachable!()
                    }
                }, if self.tcp_stealer_api.is_some() => match message {
                    Ok(StealerMessage::TcpSteal(message)) => self.respond(DaemonMessage::TcpSteal(message)).await?,
                    Ok(StealerMessage::LogMessage(log)) => self.respond(DaemonMessage::LogMessage(log)).await?,
                    Err(e) => break e,
                },
                message = self.tcp_outgoing_api.recv_from_task() => match message {
                    Ok(message) => self.respond(DaemonMessage::TcpOutgoing(message)).await?,
                    Err(e) => break e,
                },
                message = self.udp_outgoing_api.recv_from_task() => match message {
                    Ok(message) => self.respond(DaemonMessage::UdpOutgoing(message)).await?,
                    Err(e) => break e,
                },
                message = self.dns_api.recv() => match message {
                    Ok(message) => self.respond(DaemonMessage::GetAddrInfoResponse(message)).await?,
                    Err(e) => break e,
                },
                // message = self.vpn_api.daemon_message() => match message{
                //     Ok(message) => self.respond(DaemonMessage::Vpn(message)).await?,
                //     Err(e) => break e,
                // },
                _ = cancellation_token.cancelled() => return Ok(()),
            }
        };

        if let Err(e) = self.respond(DaemonMessage::Close(error.to_string())).await {
            error!("Failed to send error to client: {e:?}");
        }

        Err(error)
    }

    /// Sends a [`DaemonMessage`] response to the connected client (`mirrord-layer`).
    #[tracing::instrument(level = "trace", skip(self))]
    async fn respond(&mut self, response: DaemonMessage) -> AgentResult<()> {
        self.connection.send(response).await.map_err(Into::into)
    }

    /// Handles incoming messages from the connected client (`mirrord-layer`).
    ///
    /// Returns `false` if the client disconnected.
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::DEBUG))]
    async fn handle_client_message(&mut self, message: ClientMessage) -> AgentResult<bool> {
        match message {
            ClientMessage::FileRequest(req) => {
                if let Some(response) = self.file_manager.handle_message(req)? {
                    self.respond(DaemonMessage::File(response))
                        .await
                        .inspect_err(|fail| {
                            error!(
                                "handle_client_message -> Failed responding to file message {:#?}!",
                                fail
                            )
                        })?
                }
            }
            ClientMessage::TcpOutgoing(layer_message) => {
                self.tcp_outgoing_api.send_to_task(layer_message).await?
            }
            ClientMessage::UdpOutgoing(layer_message) => {
                self.udp_outgoing_api.send_to_task(layer_message).await?
            }
            ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
                env_vars_select,
            }) => {
                debug!(
                    "ClientMessage::GetEnvVarsRequest client id {:?} filter {:?} select {:?}",
                    self.id, env_vars_filter, env_vars_select
                );

                let env_vars_result =
                    env::select_env_vars(&self.state.env, env_vars_filter, env_vars_select);

                self.respond(DaemonMessage::GetEnvVarsResponse(env_vars_result))
                    .await?
            }
            ClientMessage::GetAddrInfoRequest(request) => {
                self.dns_api
                    .make_request(ClientGetAddrInfoRequest::V1(request))
                    .await?;
            }
            ClientMessage::GetAddrInfoRequestV2(request) => {
                self.dns_api
                    .make_request(ClientGetAddrInfoRequest::V2(request))
                    .await?;
            }
            ClientMessage::Ping => self.respond(DaemonMessage::Pong).await?,
            ClientMessage::Tcp(message) => {
                if let Some(sniffer_api) = &mut self.tcp_sniffer_api {
                    sniffer_api.handle_client_message(message).await?
                } else {
                    self.respond(DaemonMessage::Close(
                        "component responsible for mirroring incoming traffic is not running, \
                        which might be due to Kubernetes node kernel version <4.20. \
                        Check agent logs for errors and please report a bug if kernel version >=4.20".into(),
                    )).await?;
                }
            }
            ClientMessage::TcpSteal(message) => {
                if let Some(tcp_stealer_api) = self.tcp_stealer_api.as_mut() {
                    tcp_stealer_api.handle_client_message(message).await?
                } else {
                    self.respond(DaemonMessage::Close(
                        "incoming traffic stealing is not available in the targetless mode".into(),
                    ))
                    .await?;
                }
            }
            ClientMessage::Close => {
                return Ok(false);
            }
            ClientMessage::PauseTargetRequest(_) => {
                self.respond(DaemonMessage::Close(
                    "Pause isn't supported anymore.".to_string(),
                ))
                .await?;
            }
            ClientMessage::SwitchProtocolVersion(client_version) => {
                let settled_version = (&*mirrord_protocol::VERSION).min(&client_version).clone();

                self.protocol_version.replace(client_version);

                self.respond(DaemonMessage::SwitchProtocolVersionResponse(
                    settled_version,
                ))
                .await?;
            }
            ClientMessage::ReadyForLogs => {
                self.ready_for_logs = true;
            }
            ClientMessage::Vpn(_message) => {
                self.respond(DaemonMessage::Close("VPN is not supported".into()))
                    .await?;
            }
        }

        Ok(true)
    }
}

/// Upon first client connection, immediately sends [`DaemonMessage::Close`] to the client due to
/// the presence of dirty IP tables.
pub async fn notify_client_about_dirty_iptables(
    listener: TcpListener,
    communication_timeout: u16,
    tls_connector: Option<AgentTlsConnector>,
) -> AgentResult<()> {
    // WARNING: `wait_for_agent_startup` in `mirrord/kube/src/api/container.rs` expects a line
    // containing "agent_ready" to be printed. If you change this then mirrord fails to
    // initialize.
    println!("agent ready - version {}", env!("CARGO_PKG_VERSION"));

    // We wait for the first client until `communication_timeout` elapses.
    let first_connection = timeout(
        Duration::from_secs(communication_timeout.into()),
        listener.accept(),
    )
    .await;

    // Attempt to send [`DaemonMessage::Close`]. Otherwise, the agent will still fail (but
    // ungracefully).
    match first_connection {
        Ok(Ok((stream, ..))) => {
            let mut connection = ClientConnection::new(stream, 0, tls_connector).await?;
            connection
                .send(DaemonMessage::Close(
                    "Detected dirty iptables. Either some other mirrord agent is running \
            or the previous agent failed to clean up before exit. \
            If no other mirrord agent is targeting this pod, please delete the pod."
                        .to_string(),
                ))
                .await?;
        }

        Ok(Err(error)) => {
            tracing::warn!(
                ?error,
                "notify_client_about_dirty_iptables -> Failed to accept first connection"
            );
            Err(error)?
        }

        Err(..) => {
            tracing::warn!(
                "notify_client_about_dirty_iptables -> Failed to accept first connection: timeout"
            );
            Err(AgentError::FirstConnectionTimeout)?
        }
    }

    Ok(())
}

/// Real mirrord-agent routine.
///
/// Obtains the PID of the target container (if there is any),
/// starts background tasks and listens for client connections.
#[tracing::instrument(level = Level::TRACE, ret, err)]
async fn start_agent(args: Args) -> AgentResult<()> {
    trace!("start_agent -> Starting agent with args: {args:?}");

    // listen for client connections
    let ipv4_listener_result = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        args.communicate_port,
    ))
    .await;

    let listener = if args.ipv6 && ipv4_listener_result.is_err() {
        debug!("IPv6 Support enabled, and IPv4 bind failed, binding IPv6 listener");
        TcpListener::bind(SocketAddrV6::new(
            Ipv6Addr::UNSPECIFIED,
            args.communicate_port,
            0,
            0,
        ))
        .await
    } else {
        ipv4_listener_result
    }?;

    let client_listener_address = listener.local_addr()?;

    debug!(%client_listener_address, "Created the client listener.");

    let state = State::new(&args).await?;

    // check that chain names won't conflict with another agent or failed cleanup
    let leftover_rules = state
        .network_runtime
        .spawn(async move {
            let rules_v4 =
                SafeIpTables::list_mirrord_rules(&IPTablesWrapper::from(new_iptables())).await?;
            let rules_v6 = if args.ipv6 {
                SafeIpTables::list_mirrord_rules(&IPTablesWrapper::from(new_ip6tables())).await?
            } else {
                vec![]
            };

            Ok::<_, IPTablesError>([rules_v4, rules_v6].concat())
        })
        .await
        .map_err(|error| AgentError::IPTablesSetupError(error.into()))?
        .map_err(|error| AgentError::IPTablesSetupError(error.into()))?;

    if !leftover_rules.is_empty() {
        error!(
            leftover_rules = ?leftover_rules,
            "Detected dirty iptables. Either some other mirrord agent is running \
            or the previous agent failed to clean up before exit. \
            If no other mirrord agent is targeting this pod, please delete the pod. \
            If you'd like to have concurrent work consider using the operator available in mirrord for Teams."
        );
        let _ = notify_client_about_dirty_iptables(
            listener,
            args.communication_timeout,
            state.tls_connector.clone(),
        )
        .await;
        return Err(AgentError::IPTablesDirty);
    }

    let cancellation_token = CancellationToken::new();

    // To make sure that background tasks are cancelled when we exit early from this function.
    let cancel_guard = cancellation_token.clone().drop_guard();

    if let Some(metrics_address) = args.metrics {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            start_metrics(metrics_address, cancellation_token.clone())
                .await
                .inspect_err(|fail| {
                    tracing::error!(?fail, "Failed starting metrics server!");
                    cancellation_token.cancel();
                })
        });
    }

    let sniffer = if state.container_pid().is_some() {
        setup::start_sniffer(&args, &state.network_runtime, cancellation_token.clone()).await
    } else {
        BackgroundTask::Disabled
    };
    let stealer = match state.container_pid() {
        None => BackgroundTask::Disabled,
        Some(pid) => {
            let steal_handle = setup::start_traffic_redirector(
                &state.network_runtime,
                state
                    .is_with_mesh_exclusion()
                    .then(|| client_listener_address.port()),
            )
            .await?;
            setup::start_stealer(
                &state.network_runtime,
                pid,
                steal_handle,
                cancellation_token.clone(),
            )
        }
    };
    let dns = setup::start_dns(&args, &state.network_runtime, cancellation_token.clone());
    let bg_tasks = BackgroundTasks {
        sniffer,
        stealer,
        dns,
    };

    // WARNING: `wait_for_agent_startup` in `mirrord/kube/src/api/container.rs` expects a line
    // containing "agent_ready" to be printed. If you change this then mirrord fails to
    // initialize.
    println!("agent ready - version {}", env!("CARGO_PKG_VERSION"));

    let mut clients: JoinSet<ClientId> = JoinSet::new();

    // We wait for the first client until `communication_timeout` elapses.
    let first_connection = timeout(
        Duration::from_secs(args.communication_timeout.into()),
        listener.accept(),
    )
    .await;

    match first_connection {
        Ok(Ok((stream, addr))) => {
            trace!(peer = %addr, "start_agent -> First connection accepted");
            clients.spawn(state.clone().serve_client_connection(
                stream,
                bg_tasks.clone(),
                cancellation_token.clone(),
            ));
        }

        Ok(Err(error)) => {
            error!(?error, "start_agent -> Failed to accept first connection");
            Err(error)?
        }

        Err(..) => {
            error!("start_agent -> Failed to accept first connection: timeout");
            Err(AgentError::FirstConnectionTimeout)?
        }
    }

    if args.test_error {
        Err(AgentError::TestError)?
    }

    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                trace!(peer = %addr, "start_agent -> Connection accepted");
                clients.spawn(state
                    .clone()
                    .serve_client_connection(
                        stream,
                        bg_tasks.clone(),
                        cancellation_token.clone()
                    )
                );
            },

            client = clients.join_next() => {
                match client {
                    Some(Ok(client)) => {
                        trace!(client, "start_agent -> Client finished");
                    }

                    Some(Err(error)) => {
                        error!(?error, "start_agent -> Failed to join client handler task");
                    }

                    None => {
                        trace!("start_agent -> All clients finished, exiting main agent loop");
                        break
                    }
                }
            }
        }
    }

    trace!("start_agent -> Agent shutting down, dropping cancellation token for background tasks");
    mem::drop(cancel_guard);

    let BackgroundTasks {
        sniffer,
        stealer,
        dns,
    } = bg_tasks;

    tokio::join!(
        async move {
            if let BackgroundTask::Running(status, _) = sniffer {
                if let Err(error) = status.wait().await {
                    error!("start_agent -> {error}");
                }
            }
        },
        async move {
            if let BackgroundTask::Running(status, _) = stealer {
                if let Err(error) = status.wait().await {
                    error!("start_agent -> {error}");
                }
            }
        },
        async move {
            if let BackgroundTask::Running(status, _) = dns {
                if let Err(error) = status.wait().await {
                    error!("start_agent -> {error}");
                }
            }
        },
    );

    trace!("start_agent -> Agent shutdown");

    Ok(())
}

async fn clear_iptable_chain(
    ipv6_enabled: bool,
    with_mesh_exclusion: bool,
) -> Result<(), IPTablesError> {
    let v4_result: Result<(), IPTablesError> = try {
        let ipt = IPTablesWrapper::from(new_iptables());
        if SafeIpTables::list_mirrord_rules(&ipt).await?.is_empty() {
            trace!("No iptables mirrord rules found, skipping iptables cleanup.");
        } else {
            let tables = SafeIpTables::load(ipt, false, with_mesh_exclusion).await?;
            tables.cleanup().await?
        }
    };

    let v6_result: Result<(), IPTablesError> = if ipv6_enabled {
        try {
            let ipt = IPTablesWrapper::from(new_ip6tables());
            if SafeIpTables::list_mirrord_rules(&ipt).await?.is_empty() {
                trace!("No ip6tables mirrord rules found, skipping ip6tables cleanup.");
            } else {
                let tables = SafeIpTables::load(ipt, true, with_mesh_exclusion).await?;
                tables.cleanup().await?
            }
        }
    } else {
        Ok(())
    };

    v4_result.and(v6_result)
}

/// Runs the current binary as a child process,
/// using the exact same command line.
///
/// When this future is aborted before completion, the child process is automatically killed.
async fn run_child_agent() -> AgentResult<()> {
    let command_args = std::env::args().collect::<Vec<_>>();
    let (command, args) = command_args
        .split_first()
        .expect("cannot spawn child agent: command missing from program arguments");

    let mut child_agent = Command::new(command)
        .env(CHILD_PROCESS_ENV, "true")
        .args(args)
        .kill_on_drop(true)
        .spawn()?;

    let status = child_agent.wait().await?;
    if !status.success() {
        Err(AgentError::AgentFailed(status))
    } else {
        Ok(())
    }
}

/// Targeted agent's parent process.
///
/// Spawns the main agent routine in the child process and handles cleanup of iptables
/// when the child process exits.
///
/// Captures SIGTERM signals sent by Kubernetes when the pod is being gracefully deleted.
/// When a signal is captured, the child process is killed and the iptables are cleaned.
async fn start_iptable_guard(args: Args) -> AgentResult<()> {
    debug!("start_iptable_guard -> Initializing iptable-guard.");

    let state = State::new(&args).await?;
    let pid = state.container_pid();
    let with_mesh_exclusion = state.is_with_mesh_exclusion();

    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;

    let result = tokio::select! {
        _ = sigterm.recv() => {
            debug!("start_iptable_guard -> SIGTERM received, killing agent process");
            Ok(())
        }

        result = run_child_agent() => match result {
            Err(AgentError::AgentFailed(status)) if status.code() == Some(IPTABLES_DIRTY_EXIT_CODE as i32) => {
                // Err status `IPTABLES_DIRTY_EXIT_CODE` means dirty IP tables detected, skip cleanup
                tracing::warn!("dirty IP tables, cleanup skipped");
                return result;
            }
            _ => result,
        },
    };

    let Some(pid) = pid else {
        return result;
    };

    let runtime = RemoteRuntime::new_in_namespace(pid, NamespaceType::Net).await?;
    runtime
        .spawn(clear_iptable_chain(args.ipv6, with_mesh_exclusion))
        .await
        .map_err(|error| AgentError::BackgroundTaskFailed {
            task: "IPTablesCleaner",
            error: Arc::new(error),
        })?
        .map_err(|error| AgentError::BackgroundTaskFailed {
            task: "IPTablesCleaner",
            error: Arc::new(error),
        })?;

    result
}

/// mirrord-agent entrypoint.
///
/// Installs a default [`CryptoProvider`](rustls::crypto::CryptoProvider) and initializes tracing.
///
/// # Flow
///
/// If the agent is targetless, it goes straight to spawning background tasks and listening for
/// client connections.
///
/// If the agent has a target, the flow is a bit different.
/// This is because we might need to redirect incoming traffic.
///
/// Note that the agent uses static IP tables chain names, and running multiple agents at the same
/// time will cause an error.
///
/// The agent spawns a child process with the exact same command line,
/// and waits for a SIGTERM signal. When the signal is received or the child process fails,
/// the agent cleans the iptables (based on the previously set environment variables) before
/// exiting.
///
/// The child process is the real agent, which spawns background tasks and listens for client
/// connections. The child process knowns is the real agent, because it has the environment
/// variables set.
///
/// This weird flow is a safety measure - should the real agent OOM (which means instant process
/// termination) or be killed with a signal, the parent will a chance to clean iptables. If we leave
/// the iptables dirty, the whole target pod is broken, probably forever.
pub async fn main() -> AgentResult<()> {
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install crypto provider");

    if envs::JSON_LOG.from_env_or_default() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .json(),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .pretty()
                    .with_line_number(true),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    debug!(
        "main -> Initializing mirrord-agent, version {}.",
        env!("CARGO_PKG_VERSION")
    );

    let args = cli::parse_args();
    let second_process = std::env::var(CHILD_PROCESS_ENV).is_ok();

    if args.mode.is_targetless() || second_process {
        start_agent(args).await
    } else {
        start_iptable_guard(args).await
    }
}
