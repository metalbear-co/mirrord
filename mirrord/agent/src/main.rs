#![feature(result_option_inspect)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]

use nix::unistd::Pid;
use nix::sys::signal::{self, Signal};

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use actix_codec::Framed;
use cli::parse_args;
use dns::{dns_worker, DnsRequest};
use error::{AgentError, Result};
use file::FileManager;
use futures::{
    stream::{FuturesUnordered, StreamExt},
    SinkExt, TryFutureExt,
};
use libc::pid_t;
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp, LayerTcpSteal},
    ClientMessage, DaemonCodec, DaemonMessage, GetEnvVarsRequest,
};
use outgoing::{udp::UdpOutgoingApi, TcpOutgoingApi};
use sniffer::{SnifferCommand, TCPSnifferAPI, TcpConnectionSniffer};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    runtime::get_container_pid,
    steal::steal_worker,
    util::{run_thread, ClientID, IndexAllocator},
};

mod cli;
mod dns;
mod env;
mod error;
mod file;
mod outgoing;
mod runtime;
mod sniffer;
mod steal;
mod util;

const CHANNEL_SIZE: usize = 1024;

#[derive(Debug)]
struct State {
    pub clients: HashSet<ClientID>,
    index_allocator: IndexAllocator<ClientID>,
}

impl State {
    pub fn new() -> State {
        State {
            clients: HashSet::new(),
            index_allocator: IndexAllocator::new(),
        }
    }

    pub fn generate_id(&mut self) -> Option<ClientID> {
        self.index_allocator.next_index()
    }

    pub fn remove_client(&mut self, client_id: ClientID) {
        self.clients.remove(&client_id);
        self.index_allocator.free_index(client_id)
    }
}

struct ClientConnectionHandler {
    /// Used to prevent closing the main loop [`ClientConnectionHandler::start`] when any
    /// request is done (tcp outgoing feature). Stays `true` until `agent` receives an
    /// `ExitRequest`.
    id: ClientID,
    /// Handles mirrord's file operations, see [`FileManager`].
    file_manager: FileManager,
    stream: Framed<TcpStream, DaemonCodec>,
    pid: Option<u64>,
    tcp_sniffer_api: TCPSnifferAPI,
    tcp_stealer_sender: Sender<LayerTcpSteal>,
    tcp_stealer_receiver: Receiver<DaemonTcp>,
    tcp_outgoing_api: TcpOutgoingApi,
    udp_outgoing_api: UdpOutgoingApi,
    dns_sender: Sender<DnsRequest>,
}

impl ClientConnectionHandler {
    /// Initializes [`ClientConnectionHandler`].
    pub async fn new(
        id: ClientID,
        stream: TcpStream,
        pid: Option<u64>,
        ephemeral: bool,
        sniffer_command_sender: Sender<SnifferCommand>,
        dns_sender: Sender<DnsRequest>,
    ) -> Result<Self> {
        let file_manager = match pid {
            Some(_) => FileManager::new(pid),
            None if ephemeral => FileManager::new(Some(1)),
            None => FileManager::new(None),
        };
        let stream = actix_codec::Framed::new(stream, DaemonCodec::new());

        let (tcp_sender, tcp_receiver) = mpsc::channel(CHANNEL_SIZE);
        let tcp_sniffer_api =
            TCPSnifferAPI::new(id, sniffer_command_sender, tcp_receiver, tcp_sender).await?;
        let (tcp_steal_layer_sender, tcp_steal_layer_receiver) = mpsc::channel(CHANNEL_SIZE);
        let (tcp_steal_daemon_sender, tcp_steal_daemon_receiver) = mpsc::channel(CHANNEL_SIZE);

        let _ = run_thread(async move {
            if let Err(err) =
                steal_worker(tcp_steal_layer_receiver, tcp_steal_daemon_sender, pid).await
            {
                error!("mirrord-agent: `steal_worker` failed with {:?}", err)
            }
        });

        let tcp_outgoing_api = TcpOutgoingApi::new(pid);
        let udp_outgoing_api = UdpOutgoingApi::new(pid);

        let client_handler = ClientConnectionHandler {
            id,
            file_manager,
            stream,
            pid,
            tcp_sniffer_api,
            tcp_stealer_receiver: tcp_steal_daemon_receiver,
            tcp_stealer_sender: tcp_steal_layer_sender,
            tcp_outgoing_api,
            udp_outgoing_api,
            dns_sender,
        };

        Ok(client_handler)
    }

    /// Starts a loop that handles client connection and state.
    ///
    /// Breaks upon receiver/sender drop.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn start(mut self, cancellation_token: CancellationToken) -> Result<()> {
        let mut running = true;
        while running {
            select! {
                message = self.stream.next() => {
                    if let Some(message) = message {
                        running = self.handle_client_message(message?).await?;
                    } else {
                        debug!("Client {} disconnected", self.id);
                        break;
                    }
                },
                message = self.tcp_sniffer_api.recv() => {
                    if let Some(message) = message {
                        self.respond(DaemonMessage::Tcp(message)).await?;
                    } else {
                        error!("tcp sniffer stopped?");
                        break;
                    }
                },
                message = self.tcp_stealer_receiver.recv() => {
                    if let Some(message) = message {
                        self.stream.send(DaemonMessage::TcpSteal(message)).await?;
                    } else {
                        error!("tcp stealer stopped?");
                        break;
                    }
                },
                message = self.tcp_outgoing_api.daemon_message() => {
                    self.respond(DaemonMessage::TcpOutgoing(message?)).await?;
                },
                message = self.udp_outgoing_api.daemon_message() => {
                    self.respond(DaemonMessage::UdpOutgoing(message?)).await?;
                },
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Sends a [`DaemonMessage`] response to the connected client (`mirrord-layer`).
    #[tracing::instrument(level = "trace", skip(self))]
    async fn respond(&mut self, response: DaemonMessage) -> Result<()> {
        Ok(self.stream.send(response).await?)
    }

    /// Handles incoming messages from the connected client (`mirrord-layer`).
    ///
    /// Returns `false` if the client disconnected.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_client_message(&mut self, message: ClientMessage) -> Result<bool> {
        match message {
            ClientMessage::FileRequest(req) => {
                let response = self.file_manager.handle_message(req)?;
                self.respond(DaemonMessage::File(response))
                    .await
                    .inspect_err(|fail| {
                        error!(
                            "handle_client_message -> Failed responding to file message {:#?}!",
                            fail
                        )
                    })?
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

                let pid = self
                    .pid
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "self".to_string());
                let environ_path = PathBuf::from("/proc").join(pid).join("environ");

                let env_vars_result =
                    env::select_env_vars(environ_path, env_vars_filter, env_vars_select).await;

                self.respond(DaemonMessage::GetEnvVarsResponse(env_vars_result))
                    .await?
            }
            ClientMessage::GetAddrInfoRequest(request) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let dns_request = DnsRequest::new(request, tx);
                self.dns_sender.send(dns_request).await?;

                trace!("waiting for answer from dns thread");
                let response = rx.await?;

                trace!("GetAddrInfoRequest -> response {:#?}", response);

                self.respond(DaemonMessage::GetAddrInfoResponse(response))
                    .await?
            }
            ClientMessage::Ping => self.respond(DaemonMessage::Pong).await?,
            ClientMessage::Tcp(message) => self.handle_client_tcp(message).await?,
            ClientMessage::TcpSteal(message) => self.tcp_stealer_sender.send(message).await?,
            ClientMessage::Close => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn handle_client_tcp(&mut self, message: LayerTcp) -> Result<()> {
        match message {
            LayerTcp::PortSubscribe(port) => self.tcp_sniffer_api.subscribe(port).await,
            LayerTcp::ConnectionUnsubscribe(connection_id) => {
                self.tcp_sniffer_api
                    .connection_unsubscribe(connection_id)
                    .await
            }
            LayerTcp::PortUnsubscribe(port) => self.tcp_sniffer_api.port_unsubscribe(port).await,
        }
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

    let pid = match (args.container_id, args.container_runtime) {
        (Some(container_id), Some(container_runtime)) => {
            Some(get_container_pid(&container_id, &container_runtime).await?)
        }
        _ => None,
    };

    let mut state = State::new();
    let cancellation_token = CancellationToken::new();
    // Cancel all other tasks on exit
    let cancel_guard = cancellation_token.clone().drop_guard();
    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);
    let (dns_sender, dns_receiver) = mpsc::channel(1000);
    let _ = run_thread(dns_worker(dns_receiver, pid));

    let sniffer_cancellation_token = cancellation_token.clone();
    let sniffer_task = run_thread(
        TcpConnectionSniffer::new(sniffer_command_rx, pid, args.network_interface)
            .and_then(|sniffer| sniffer.start(sniffer_cancellation_token)),
    );


    pid.inspect(|pid| {
        signal::kill(Pid::from_raw(pid.clone() as pid_t), Signal::SIGSTOP).unwrap();
    });

    // WARNING: This exact string is expected to be read in `pod_api.rs`, more specifically in
    // `wait_for_agent_startup`. If you change this, or if this is not logged (i.e. user disables
    // `MIRRORD_AGENT_RUST_LOG`), then mirrord fails to initialize.
    info!("agent ready");

    let mut clients = FuturesUnordered::new();
    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                trace!("start -> Connection accepted from {:?}", addr);

                if let Some(client_id) = state.generate_id() {

                    state.clients.insert(client_id);
                    let sniffer_command_tx = sniffer_command_tx.clone();
                    let cancellation_token = cancellation_token.clone();
                    let dns_sender = dns_sender.clone();
                    let client = tokio::spawn(async move {
                        match ClientConnectionHandler::new(
                                client_id,
                                stream,
                                pid,
                                args.ephemeral_container,
                                sniffer_command_tx,
                                dns_sender,
                            )
                            .and_then(|client| client.start(cancellation_token))
                            .await
                            {
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
                    clients.push(client);
                } else {
                    error!("start_client -> Ran out of connections, dropping new connection");
                }

            },
            Some(client) = clients.next() => {
                let client_id = client?;
                state.remove_client(client_id);
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(args.communication_timeout.into())) => {
                if state.clients.is_empty() {
                    trace!("Main thread timeout, no clients connected.");
                    break;
                }
            }
        }
    }

    // Resume original container when agent is done.
    // TODO: remove first inspect
    pid.inspect(|pid| info!("pid: {}", pid)).inspect(|pid| {
        signal::kill(Pid::from_raw(pid.clone() as pid_t), Signal::SIGCONT).unwrap();
    });


    trace!("Agent shutting down.");
    drop(cancel_guard);
    if let Err(err) = sniffer_task.join().map_err(|_| AgentError::JoinTask)? {
        error!("start_agent -> sniffer task failed with error: {}", err);
    }

    trace!("Agent shutdown.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_span_events(FmtSpan::ACTIVE)
                .compact(),
        )
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    debug!("main -> Initializing mirrord-agent.");

    match start_agent().await {
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
