#![feature(result_option_inspect)]
#![feature(hash_drain_filter)]
#![feature(let_chains)]
#![feature(type_alias_impl_trait)]
#![feature(tcp_quickack)]
#![feature(async_fn_in_trait)]
#![feature(lazy_cell)]
#![allow(incomplete_features)]
#![warn(clippy::indexing_slicing)]

use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use actix_codec::Framed;
use cli::parse_args;
use dns::DnsApi;
use error::{AgentError, Result};
use file::FileManager;
use futures::{
    stream::{FuturesUnordered, StreamExt},
    SinkExt, TryFutureExt,
};
use mirrord_protocol::{
    pause::DaemonPauseTarget, ClientMessage, DaemonCodec, DaemonMessage, GetEnvVarsRequest,
    LogMessage,
};
use outgoing::{udp::UdpOutgoingApi, TcpOutgoingApi};
use sniffer::{SnifferCommand, TcpConnectionSniffer, TcpSnifferApi};
use steal::api::TcpStealerApi;
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
    task::JoinHandle,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    cli::Args,
    container_handle::ContainerHandle,
    runtime::get_container,
    steal::{
        connection::TcpConnectionStealer,
        ip_tables::{
            IPTablesWrapper, SafeIpTables, IPTABLE_MESH, IPTABLE_MESH_ENV, IPTABLE_PREROUTING,
            IPTABLE_PREROUTING_ENV, IPTABLE_STANDARD, IPTABLE_STANDARD_ENV,
        },
        StealerCommand,
    },
    util::{run_thread_in_namespace, ClientId, IndexAllocator},
    watched_task::{TaskStatus, WatchedTask},
};

mod cgroup;
mod cli;
mod container_handle;
mod dns;
mod env;
mod error;
mod file;
mod namespace;
mod outgoing;
mod runtime;
mod sniffer;
mod steal;
mod util;
mod watched_task;

const CHANNEL_SIZE: usize = 1024;

/// Keeps track of connected clients.
#[derive(Debug)]
struct State {
    clients: HashSet<ClientId>,
    index_allocator: IndexAllocator<ClientId, 100>,
    /// Handle to the target container if there is one.
    /// This is optional because it is acceptable not to pass the container runtime and id if not
    /// pausing. When those args are not passed, container is [`None`].
    container: Option<ContainerHandle>,
    env: HashMap<String, String>,
    ephemeral: bool,
}

impl State {
    /// Return [`Err`] if container runtime operations failed.
    pub async fn new(args: &Args) -> Result<State> {
        let container = get_container(
            args.container_id.as_ref(),
            args.container_runtime.as_ref(),
            args.ephemeral_container,
        )
        .await?;

        let container = match container {
            Some(container) => Some(ContainerHandle::new(container).await?),
            None => None,
        };

        // If we are in an ephemeral container, we use pid 1.
        // if not, we use the pid of the target container or fallback to self
        let pid = container
            .as_ref()
            .map(|h| h.pid().to_string())
            .unwrap_or_else(|| "self".to_string());

        let mut env: HashMap<String, String> = HashMap::new();

        let environ_path = PathBuf::from("/proc").join(pid).join("environ");

        if let Some(container) = container.as_ref() {
            env.extend(container.raw_env().clone());
        }

        match env::get_proc_environ(environ_path).await {
            Ok(environ) => env.extend(environ.into_iter()),
            Err(err) => {
                error!("Failed to get process environment variables: {err:?}");
            }
        };

        Ok(State {
            clients: HashSet::new(),
            index_allocator: Default::default(),
            container,
            env,
            ephemeral: args.ephemeral_container,
        })
    }

    /// Return the process ID of the target container if there is one.
    pub fn container_pid(&self) -> Option<u64> {
        self.container.as_ref().map(ContainerHandle::pid)
    }

    /// If there are [`ClientId`]s left, insert new one and return it.
    pub async fn new_client(&mut self) -> Result<ClientId> {
        let new_id = self
            .index_allocator
            .next_index()
            .ok_or(AgentError::ConnectionLimitReached)?;
        self.clients.insert(new_id);
        Ok(new_id)
    }

    pub async fn new_connection(
        &mut self,
        stream: TcpStream,
        tasks: BackgroundTasks,
        cancellation_token: CancellationToken,
        protocol_version: semver::Version,
    ) -> Result<Option<JoinHandle<u32>>> {
        let mut stream = Framed::new(stream, DaemonCodec::new());

        let client_id = match self.new_client().await {
            Ok(id) => id,
            Err(err) => {
                let _ = stream.send(DaemonMessage::Close(err.to_string())).await; // Ignore message send error.

                if let AgentError::ConnectionLimitReached = err {
                    error!("{err}");
                    return Ok(None);
                } else {
                    // Propagate all errors that are not ConnectionLimitReached.
                    Err(err)?
                }
            }
        };

        let container_handle = self.container.clone();
        let ephemeral_container = self.ephemeral;
        let env = self.env.clone();

        let task = tokio::spawn(async move {
            let result = ClientConnectionHandler::new(
                client_id,
                stream,
                container_handle,
                ephemeral_container,
                tasks,
                env,
                protocol_version,
            )
            .and_then(|client| client.start(cancellation_token))
            .await;

            match result {
                Ok(_) => {
                    trace!(
                        "ClientConnectionHandler::start -> Client {} disconnected",
                        client_id
                    );
                }
                Err(e) => {
                    error!(
                        "ClientConnectionHandler::start -> Client {} disconnected with error: {}",
                        client_id, e
                    );
                }
            }

            client_id
        });

        Ok(Some(task))
    }

    /// Free id of the given client.
    pub async fn remove_client(&mut self, client_id: ClientId) -> Result<()> {
        self.clients.remove(&client_id);
        self.index_allocator.free_index(client_id);

        Ok(())
    }
}

/// Handles to background tasks used by [`ClientConnectionHandler`].
#[derive(Clone)]
struct BackgroundTasks {
    sniffer_status: TaskStatus,
    sniffer_sender: Sender<SnifferCommand>,
    stealer_status: TaskStatus,
    stealer_sender: Sender<StealerCommand>,
    dns_api: DnsApi,
}

struct ClientConnectionHandler {
    id: ClientId,
    /// Handles mirrord's file operations, see [`FileManager`].
    file_manager: FileManager,
    stream: Framed<TcpStream, DaemonCodec>,
    tcp_sniffer_api: Option<TcpSnifferApi>,
    tcp_stealer_api: TcpStealerApi,
    tcp_outgoing_api: TcpOutgoingApi,
    udp_outgoing_api: UdpOutgoingApi,
    dns_api: DnsApi,
    env: HashMap<String, String>,
    /// Handle to the target container if there is one.
    /// Used for pausing the container.
    container_handle: Option<ContainerHandle>,
}

impl ClientConnectionHandler {
    /// Initializes [`ClientConnectionHandler`].
    pub async fn new(
        id: ClientId,
        mut stream: Framed<TcpStream, DaemonCodec>,
        container_handle: Option<ContainerHandle>,
        ephemeral: bool,
        bg_tasks: BackgroundTasks,
        env: HashMap<String, String>,
        protocol_version: semver::Version,
    ) -> Result<Self> {
        let pid = container_handle.as_ref().map(ContainerHandle::pid);

        let file_manager = FileManager::new(pid.or_else(|| ephemeral.then_some(1)));

        let tcp_sniffer_api = match TcpSnifferApi::new(
            id,
            bg_tasks.sniffer_sender,
            bg_tasks.sniffer_status,
            CHANNEL_SIZE,
        )
        .await
        {
            Ok(api) => Some(api),
            Err(e) => {
                let message = format!(
                    "Failed to create TcpSnifferApi: {e}, this could be due to kernel version."
                );
                warn!(message);
                let _ = stream
                    .send(DaemonMessage::LogMessage(LogMessage::warn(message)))
                    .await; // Ignore message send error.

                None
            }
        };

        let tcp_stealer_api = match TcpStealerApi::new(
            id,
            bg_tasks.stealer_sender,
            bg_tasks.stealer_status,
            CHANNEL_SIZE,
            protocol_version,
        )
        .await
        {
            Ok(api) => api,
            Err(e) => {
                let _ = stream
                    .send(DaemonMessage::Close(format!(
                        "Failed to create TcpStealerApi: {e}."
                    )))
                    .await; // Ignore message send error.

                Err(e)?
            }
        };

        let tcp_outgoing_api = TcpOutgoingApi::new(pid);
        let udp_outgoing_api = UdpOutgoingApi::new(pid);

        let client_handler = ClientConnectionHandler {
            id,
            file_manager,
            stream,
            tcp_sniffer_api,
            tcp_stealer_api,
            tcp_outgoing_api,
            udp_outgoing_api,
            dns_api: bg_tasks.dns_api,
            env,
            container_handle,
        };

        Ok(client_handler)
    }

    /// Starts a loop that handles client connection and state.
    ///
    /// Breaks upon receiver/sender drop.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn start(mut self, cancellation_token: CancellationToken) -> Result<()> {
        let error = loop {
            select! {
                message = self.stream.next() => {
                    let Some(message) = message else {
                        debug!("Client {} disconnected", self.id);
                        return Ok(());
                    };

                    match self.handle_client_message(message?).await {
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
                message = self.tcp_stealer_api.recv() => match message {
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
        self.stream.send(response).await.map_err(Into::into)
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
                    env::select_env_vars(&self.env, env_vars_filter, env_vars_select);

                self.respond(DaemonMessage::GetEnvVarsResponse(env_vars_result))
                    .await?
            }
            ClientMessage::GetAddrInfoRequest(request) => {
                let response = self.dns_api.make_request(request).await?;
                trace!("GetAddrInfoRequest -> response {:#?}", response);

                self.respond(DaemonMessage::GetAddrInfoResponse(response))
                    .await?
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
                self.tcp_stealer_api.handle_client_message(message).await?
            }
            ClientMessage::Close => {
                return Ok(false);
            }
            ClientMessage::PauseTargetRequest(pause) => {
                match self
                    .container_handle
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
                self.tcp_stealer_api
                    .switch_protocol_version(version.clone())
                    .await?;

                self.respond(DaemonMessage::SwitchProtocolVersionResponse(version))
                    .await?;
            }
            ClientMessage::ReadyForLogs => {}
        }

        Ok(true)
    }
}

/// Initializes the agent's [`State`], channels, threads, and runs [`ClientConnectionHandler`]s.
#[tracing::instrument(level = "trace")]
async fn start_agent() -> Result<()> {
    let args = parse_args();
    trace!("Starting agent with args: {args:?}");

    let listener = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        args.communicate_port,
    ))
    .await?;

    let mut state = State::new(&args).await?;

    let cancellation_token = CancellationToken::new();
    // Cancel all other tasks on exit
    let cancel_guard = cancellation_token.clone().drop_guard();

    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);
    let (stealer_command_tx, stealer_command_rx) = mpsc::channel::<StealerCommand>(1000);

    let dns_api = DnsApi::new(state.container_pid(), 1000);

    let (sniffer_task, sniffer_status) = {
        let cancellation_token = cancellation_token.clone();
        let watched_task = WatchedTask::new(
            TcpConnectionSniffer::TASK_NAME,
            TcpConnectionSniffer::new(sniffer_command_rx, args.network_interface).and_then(
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

        (task, status)
    };

    let (stealer_task, stealer_status) = {
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

        (task, status)
    };

    let mut bg_tasks = BackgroundTasks {
        sniffer_status,
        sniffer_sender: sniffer_command_tx,
        stealer_status,
        stealer_sender: stealer_command_tx,
        dns_api,
    };

    // WARNING: `wait_for_agent_startup` in `mirrord/kube/src/api/container.rs` expects a line
    // containing "agent_ready" to be printed. If you change this then mirrord fails to
    // initialize.
    println!("agent ready - version {}", env!("CARGO_PKG_VERSION"));

    let mut clients = FuturesUnordered::new();

    // For the first client, we use communication_timeout, then we exit when no more
    // no connections.
    match timeout(
        Duration::from_secs(args.communication_timeout.into()),
        listener.accept(),
    )
    .await
    {
        Ok(Ok((stream, addr))) => {
            trace!("start -> Connection accepted from {:?}", addr);
            if let Some(client) = state
                .new_connection(
                    stream,
                    bg_tasks.clone(),
                    cancellation_token.clone(),
                    args.base_protocol_version.clone(),
                )
                .await?
            {
                clients.push(client)
            };
        }
        Ok(Err(err)) => {
            error!("start -> Failed to accept connection: {:?}", err);
            Err(err)?
        }
        Err(err) => {
            error!("start -> Failed to accept first connection: timeout");
            Err(err)?
        }
    }

    if args.test_error {
        Err(AgentError::TestError)?
    }

    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                trace!("start -> Connection accepted from {:?}", addr);
                if let Some(client) = state.new_connection(
                    stream,
                    bg_tasks.clone(),
                    cancellation_token.clone(),
                    args.base_protocol_version.clone(),
                ).await? {clients.push(client) };
            },
            client = clients.next() => {
                match client {
                    Some(client) => {
                        let client_id = client?;
                        state.remove_client(client_id).await?;
                    }
                    None => {
                        trace!("Main thread timeout, no clients left.");
                        break
                    }
                }
            },
        }
    }

    trace!("Agent shutting down.");
    drop(cancel_guard);

    sniffer_task.join().map_err(|_| AgentError::JoinTask)?;
    if let Some(err) = bg_tasks.sniffer_status.err().await {
        error!("start_agent -> sniffer task failed with error: {}", err);
    }

    stealer_task.join().map_err(|_| AgentError::JoinTask)?;
    if let Some(err) = bg_tasks.stealer_status.err().await {
        error!("start_agent -> stealer task failed with error: {}", err);
    }

    trace!("Agent shutdown.");
    Ok(())
}

async fn clear_iptable_chain() -> Result<()> {
    let ipt = iptables::new(false).unwrap();

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

async fn start_iptable_guard() -> Result<()> {
    debug!("start_iptable_guard -> Initializing iptable-guard.");

    let args = parse_args();
    let state = State::new(&args).await?;
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

    let agent_result = if std::env::var(IPTABLE_PREROUTING_ENV).is_ok()
        && std::env::var(IPTABLE_MESH_ENV).is_ok()
    {
        start_agent().await
    } else {
        start_iptable_guard().await
    };

    // wait for background tasks to finish
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

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
