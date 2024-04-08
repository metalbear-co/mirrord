#![feature(hash_extract_if)]
#![feature(let_chains)]
#![feature(type_alias_impl_trait)]
#![feature(tcp_quickack)]
#![feature(lazy_cell)]
#![warn(clippy::indexing_slicing)]

use std::{
    collections::HashMap,
    mem,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use dns::{DnsCommand, DnsWorker};
use futures::TryFutureExt;
use mirrord_protocol::{
    pause::DaemonPauseTarget, ClientMessage, DaemonMessage, GetEnvVarsRequest, LogMessage,
};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
    task::JoinSet,
    time::{timeout, Duration},
};
use tokio_rustls::{rustls::ClientConfig, TlsConnector};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    cli::Args,
    client_connection::ClientConnection,
    container_handle::ContainerHandle,
    dns::DnsApi,
    error::{AgentError, Result},
    file::FileManager,
    outgoing::{TcpOutgoingApi, UdpOutgoingApi},
    runtime::get_container,
    sniffer::{SnifferCommand, TcpConnectionSniffer, TcpSnifferApi},
    steal::{
        ip_tables::{
            new_iptables, IPTablesWrapper, SafeIpTables, IPTABLE_MESH, IPTABLE_MESH_ENV,
            IPTABLE_PREROUTING, IPTABLE_PREROUTING_ENV, IPTABLE_STANDARD, IPTABLE_STANDARD_ENV,
        },
        StealerCommand, TcpConnectionStealer, TcpStealerApi,
    },
    util::{run_thread_in_namespace, ClientId},
    watched_task::{TaskStatus, WatchedTask},
};

mod cgroup;
mod cli;
mod client_connection;
mod container_handle;
mod dns;
mod env;
mod error;
mod file;
mod http;
mod namespace;
mod outgoing;
mod runtime;
mod sniffer;
mod steal;
mod tls_verifier;
mod util;
mod watched_task;

/// Size of [`mpsc`] channels connecting [`TcpStealerApi`] and [`TcpSnifferApi`] with their
/// background tasks.
const CHANNEL_SIZE: usize = 1024;

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
    tls_connector: Option<TlsConnector>,
}

impl State {
    /// Return [`Err`] if container runtime operations failed.
    pub async fn new(args: &Args, watch: drain::Watch) -> Result<State> {
        let tls_connector = args.use_tls.then(|| {
            TlsConnector::from(Arc::new(
                ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(tls_verifier::NoopVerifier))
                    .with_no_client_auth(),
            ))
        });

        let mut env: HashMap<String, String> = HashMap::new();

        let (ephemeral, container, pid) = match &args.mode {
            cli::Mode::Targeted {
                container_id,
                container_runtime,
                ..
            } => {
                let container =
                    get_container(container_id.clone(), Some(container_runtime)).await?;

                let container_handle = ContainerHandle::new(container, watch).await?;
                let pid = container_handle.pid().to_string();

                env.extend(container_handle.raw_env().clone());

                (false, Some(container_handle), pid)
            }
            cli::Mode::Ephemeral { .. } => {
                let container_handle = ContainerHandle::new(
                    runtime::Container::Ephemeral(runtime::EphemeralContainer {}),
                    watch,
                )
                .await?;

                let pid = container_handle.pid().to_string();
                env.extend(container_handle.raw_env().clone());

                // If we are in an ephemeral container, we use pid 1.
                (true, Some(container_handle), pid)
            }
            cli::Mode::Targetless | cli::Mode::BlackboxTest => (false, None, "self".to_string()),
        };

        let environ_path = PathBuf::from("/proc").join(pid).join("environ");

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
        protocol_version: semver::Version,
    ) -> u32 {
        let client_id = self.next_client_id.fetch_add(1, Ordering::Relaxed);

        let result = ClientConnection::new(stream, client_id, self.tls_connector.clone())
            .map_err(AgentError::from)
            .and_then(|connection| {
                ClientConnectionHandler::new(client_id, connection, tasks, protocol_version, self)
            })
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
}

enum BackgroundTask<Command> {
    Running(TaskStatus, Sender<Command>),
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
    tcp_sniffer_api: Option<TcpSnifferApi>,
    tcp_stealer_api: Option<TcpStealerApi>,
    tcp_outgoing_api: TcpOutgoingApi,
    udp_outgoing_api: UdpOutgoingApi,
    dns_api: DnsApi,
    state: State,
}

impl ClientConnectionHandler {
    /// Initializes [`ClientConnectionHandler`].
    pub async fn new(
        id: ClientId,
        mut connection: ClientConnection,
        bg_tasks: BackgroundTasks,
        protocol_version: semver::Version,
        state: State,
    ) -> Result<Self> {
        let pid = state.container_pid();

        let file_manager = FileManager::new(pid.or_else(|| state.ephemeral.then_some(1)));

        let tcp_sniffer_api = Self::ceate_sniffer_api(id, bg_tasks.sniffer, &mut connection).await;
        let tcp_stealer_api =
            Self::ceate_stealer_api(id, bg_tasks.stealer, protocol_version, &mut connection)
                .await?;
        let dns_api = Self::create_dns_api(bg_tasks.dns);

        let tcp_outgoing_api = TcpOutgoingApi::new(pid);
        let udp_outgoing_api = UdpOutgoingApi::new(pid);

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
        };

        Ok(client_handler)
    }

    async fn ceate_sniffer_api(
        id: ClientId,
        task: BackgroundTask<SnifferCommand>,
        connection: &mut ClientConnection,
    ) -> Option<TcpSnifferApi> {
        if let BackgroundTask::Running(sniffer_status, sniffer_sender) = task {
            match TcpSnifferApi::new(id, sniffer_sender, sniffer_status, CHANNEL_SIZE).await {
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

    async fn ceate_stealer_api(
        id: ClientId,
        task: BackgroundTask<StealerCommand>,
        protocol_version: semver::Version,
        connection: &mut ClientConnection,
    ) -> Result<Option<TcpStealerApi>> {
        if let BackgroundTask::Running(stealer_status, stealer_sender) = task {
            match TcpStealerApi::new(
                id,
                stealer_sender,
                stealer_status,
                CHANNEL_SIZE,
                protocol_version,
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
    async fn start(mut self, cancellation_token: CancellationToken) -> Result<()> {
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
                    Ok(message) => self.respond(DaemonMessage::Tcp(message)).await?,
                    Err(e) => break e,
                },
                message = async {
                    if let Some(ref mut stealer_api) = self.tcp_stealer_api {
                        stealer_api.recv().await
                    } else {
                        unreachable!()
                    }
                }, if self.tcp_stealer_api.is_some() => match message {
                    Ok(message) => self.respond(DaemonMessage::TcpSteal(message)).await?,
                    Err(e) => break e,
                },
                message = self.tcp_outgoing_api.daemon_message() => match message {
                    Ok(message) => self.respond(DaemonMessage::TcpOutgoing(message)).await?,
                    Err(e) => break e,
                },
                message = self.udp_outgoing_api.daemon_message() => match message {
                    Ok(message) => self.respond(DaemonMessage::UdpOutgoing(message)).await?,
                    Err(e) => break e,
                },
                message = self.dns_api.recv() => match message {
                    Ok(message) => self.respond(DaemonMessage::GetAddrInfoResponse(message)).await?,
                    Err(e) => break e,
                },
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
    async fn respond(&mut self, response: DaemonMessage) -> Result<()> {
        self.connection.send(response).await.map_err(Into::into)
    }

    /// Handles incoming messages from the connected client (`mirrord-layer`).
    ///
    /// Returns `false` if the client disconnected.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_client_message(&mut self, message: ClientMessage) -> Result<bool> {
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
                self.tcp_outgoing_api.layer_message(layer_message).await?
            }
            ClientMessage::UdpOutgoing(layer_message) => {
                self.udp_outgoing_api.layer_message(layer_message).await?
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
                self.dns_api.make_request(request).await?;
            }
            ClientMessage::Ping => self.respond(DaemonMessage::Pong).await?,
            ClientMessage::Tcp(message) => {
                if let Some(sniffer_api) = &mut self.tcp_sniffer_api {
                    sniffer_api.handle_client_message(message).await?
                } else {
                    warn!("received tcp sniffer request while not available");
                    Err(AgentError::SnifferApiError)?
                }
            }
            ClientMessage::TcpSteal(message) => {
                if let Some(tcp_stealer_api) = self.tcp_stealer_api.as_mut() {
                    tcp_stealer_api.handle_client_message(message).await?
                } else {
                    warn!("received tcp steal request while not available");
                    Err(AgentError::SnifferApiError)?
                }
            }
            ClientMessage::Close => {
                return Ok(false);
            }
            ClientMessage::PauseTargetRequest(pause) => {
                match self
                    .state
                    .container
                    .as_ref()
                    .ok_or(AgentError::PauseAbsentTarget)?
                    .set_paused(pause)
                    .await
                {
                    Ok(changed) => {
                        self.respond(DaemonMessage::PauseTarget(
                            DaemonPauseTarget::PauseResponse {
                                changed,
                                container_paused: pause,
                            },
                        ))
                        .await?;
                    }
                    Err(e) => {
                        self.respond(DaemonMessage::LogMessage(LogMessage::error(format!(
                            "Failed to pause target container: {e:?}"
                        ))))
                        .await?;
                    }
                }
            }
            ClientMessage::SwitchProtocolVersion(version) => {
                if let Some(tcp_stealer_api) = self.tcp_stealer_api.as_mut() {
                    tcp_stealer_api
                        .switch_protocol_version(version.clone())
                        .await?;
                }

                self.respond(DaemonMessage::SwitchProtocolVersionResponse(version))
                    .await?;
            }
            ClientMessage::ReadyForLogs => {}
        }

        Ok(true)
    }
}

/// Initializes the agent's [`State`], channels, threads, and runs [`ClientConnectionHandler`]s.
#[tracing::instrument(level = "trace", ret)]
async fn start_agent(args: Args, watch: drain::Watch) -> Result<()> {
    trace!("start_agent -> Starting agent with args: {args:?}");

    let listener = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        args.communicate_port,
    ))
    .await?;

    let state = State::new(&args, watch).await?;

    let cancellation_token = CancellationToken::new();

    // To make sure that background tasks are cancelled when we exit early from this function.
    let cancel_guard = cancellation_token.clone().drop_guard();

    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);
    let (stealer_command_tx, stealer_command_rx) = mpsc::channel::<StealerCommand>(1000);
    let (dns_command_tx, dns_command_rx) = mpsc::channel::<DnsCommand>(1000);

    let (sniffer_task, sniffer_status) = if args.mode.is_targetless() {
        (None, None)
    } else {
        let cancellation_token = cancellation_token.clone();

        let mesh = args.mode.mesh();

        let watched_task = WatchedTask::new(
            TcpConnectionSniffer::TASK_NAME,
            TcpConnectionSniffer::new(sniffer_command_rx, args.network_interface, mesh).and_then(
                |sniffer| async move {
                    let res = sniffer.start(cancellation_token).await;
                    if let Err(err) = res.as_ref() {
                        error!("Sniffer failed: {err}");
                    }
                    Ok(())
                },
            ),
        );
        let status = watched_task.status();
        let task = run_thread_in_namespace(
            watched_task.start(),
            TcpConnectionSniffer::TASK_NAME.to_string(),
            state.container_pid(),
            "net",
        );

        (Some(task), Some(status))
    };

    let (stealer_task, stealer_status) = if args.mode.is_targetless() {
        (None, None)
    } else {
        let cancellation_token = cancellation_token.clone();
        let watched_task = WatchedTask::new(
            TcpConnectionStealer::TASK_NAME,
            TcpConnectionStealer::new(stealer_command_rx).and_then(|stealer| async move {
                let res = stealer.start(cancellation_token).await;
                if let Err(err) = res.as_ref() {
                    error!("Stealer failed: {err}");
                }
                res
            }),
        );
        let status = watched_task.status();
        let task = run_thread_in_namespace(
            watched_task.start(),
            TcpConnectionStealer::TASK_NAME.to_string(),
            state.container_pid(),
            "net",
        );

        (Some(task), Some(status))
    };

    let (dns_task, dns_status) = {
        let cancellation_token = cancellation_token.clone();
        let watched_task = WatchedTask::new(
            DnsWorker::TASK_NAME,
            DnsWorker::new(state.container_pid(), dns_command_rx).run(cancellation_token),
        );
        let status = watched_task.status();
        let task = run_thread_in_namespace(
            watched_task.start(),
            DnsWorker::TASK_NAME.to_string(),
            state.container_pid(),
            "net",
        );

        (task, status)
    };

    let bg_tasks = BackgroundTasks {
        sniffer: sniffer_status
            .map(|status| BackgroundTask::Running(status, sniffer_command_tx))
            .unwrap_or(BackgroundTask::Disabled),
        stealer: stealer_status
            .map(|status| BackgroundTask::Running(status, stealer_command_tx))
            .unwrap_or(BackgroundTask::Disabled),
        dns: BackgroundTask::Running(dns_status, dns_command_tx),
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
                args.base_protocol_version.clone(),
            ));
        }

        Ok(Err(error)) => {
            error!(?error, "start_agent -> Failed to accept first connection");
            Err(error)?
        }

        Err(error) => {
            error!("start_agent -> Failed to accept first connection: timeout");
            Err(error)?
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
                        cancellation_token.clone(),
                        args.base_protocol_version.clone(),
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
                        Err(error)?
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

    if let (Some(sniffer_task), BackgroundTask::Running(mut sniffer_status, _)) =
        (sniffer_task, sniffer)
    {
        sniffer_task.join().map_err(|_| AgentError::JoinTask)?;
        if let Some(err) = sniffer_status.err().await {
            error!("start_agent -> sniffer task failed with error: {}", err);
        }
    }

    if let (Some(stealer_task), BackgroundTask::Running(mut stealer_status, _)) =
        (stealer_task, stealer)
    {
        stealer_task.join().map_err(|_| AgentError::JoinTask)?;
        if let Some(err) = stealer_status.err().await {
            error!("start_agent -> stealer task failed with error: {}", err);
        }
    }

    if let BackgroundTask::Running(mut dns_status, _) = dns {
        dns_task.join().map_err(|_| AgentError::JoinTask)?;
        if let Some(err) = dns_status.err().await {
            error!("start_agent -> dns task failed with error: {}", err);
        }
    }

    trace!("start_agent -> Agent shutdown");

    Ok(())
}

async fn clear_iptable_chain() -> Result<()> {
    let ipt = new_iptables();

    SafeIpTables::load(IPTablesWrapper::from(ipt), false)
        .await?
        .cleanup()
        .await?;

    Ok(())
}

fn spawn_child_agent() -> Result<()> {
    let command_args = std::env::args().collect::<Vec<_>>();
    let (command, args) = command_args
        .split_first()
        .expect("cannot spawn child agent: command missing from program arguments");

    let mut child_agent = std::process::Command::new(command).args(args).spawn()?;

    let _ = child_agent.wait();

    Ok(())
}

async fn start_iptable_guard(args: Args, watch: drain::Watch) -> Result<()> {
    debug!("start_iptable_guard -> Initializing iptable-guard.");

    let state = State::new(&args, watch).await?;
    let pid = state.container_pid();

    std::env::set_var(IPTABLE_PREROUTING_ENV, IPTABLE_PREROUTING.as_str());
    std::env::set_var(IPTABLE_MESH_ENV, IPTABLE_MESH.as_str());
    std::env::set_var(IPTABLE_STANDARD_ENV, IPTABLE_STANDARD.as_str());

    let result = spawn_child_agent();

    let _ = run_thread_in_namespace(
        clear_iptable_chain(),
        "clear iptables".to_owned(),
        pid,
        "net",
    )
    .join()
    .map_err(|_| AgentError::JoinTask)?;

    result
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .compact(),
        )
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    debug!(
        "main -> Initializing mirrord-agent, version {}.",
        env!("CARGO_PKG_VERSION")
    );

    let args = cli::parse_args();

    let (signal, watch) = drain::channel();

    let agent_result = if args.mode.is_targetless()
        || (std::env::var(IPTABLE_PREROUTING_ENV).is_ok()
            && std::env::var(IPTABLE_MESH_ENV).is_ok())
    {
        start_agent(args, watch).await
    } else {
        start_iptable_guard(args, watch).await
    };

    // wait for background tasks/drop impl
    tokio::time::timeout(std::time::Duration::from_secs(10), signal.drain())
        .await
        .is_err()
        .then(|| {
            warn!("main -> mirrord-agent waiting for drain timed out");
        });

    match agent_result {
        Ok(_) => {
            info!("main -> mirrord-agent `start` exiting successfully.")
        }
        Err(fail) => {
            error!(
                "main -> mirrord-agent `start` exiting with error {:#?}",
                fail
            )
        }
    }

    Ok(())
}
