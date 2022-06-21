#![feature(result_option_inspect)]
#![feature(hash_drain_filter)]

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use actix_codec::Framed;
use error::AgentError;
use file::FileManager;
use futures::{stream::FuturesUnordered, SinkExt};
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp},
    ClientMessage, ConnectionID, DaemonCodec, DaemonMessage, FileError, GetEnvVarsRequest, Port,
    ResponseError,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tracing::{debug, error, info, trace};
use tracing_subscriber::prelude::*;

mod cli;
mod error;
mod file;
mod runtime;
mod sniffer;
mod util;

use cli::parse_args;
use sniffer::{Enabled, SnifferCommand, SnifferOutput, TCPConnectionSniffer, TCPSnifferAPI};
use util::{IndexAllocator, Subscriptions};

use crate::runtime::get_container_pid;

#[derive(Debug)]
struct State {
    pub agents: HashSet<AgentID>,
    index_allocator: IndexAllocator<AgentID>,
}

impl State {
    pub fn new() -> State {
        State {
            agents: HashSet::new(),
            index_allocator: IndexAllocator::new(),
        }
    }

    pub fn generate_id(&mut self) -> Option<AgentID> {
        self.index_allocator.next_index()
    }

    pub fn remove_agent(&mut self, agent_id: AgentID) {
        self.agents.remove(&agent_id);
        self.index_allocator.free_index(agent_id)
    }
}

#[derive(Debug)]
pub struct PeerMessage {
    client_message: ClientMessage,
    peer_id: AgentID,
}

/// Helper function that loads the process' environment variables, and selects only those that were
/// requested from `mirrord-layer` (ignores vars specified in `filter_env_vars`).
///
/// Returns an error if none of the requested environment variables were found.
async fn select_env_vars(
    environ_path: PathBuf,
    filter_env_vars: HashSet<String>,
    select_env_vars: HashSet<String>,
) -> Result<HashMap<String, String>, ResponseError> {
    debug!(
        "select_env_vars -> environ_path {:#?} filter_env_vars {:#?} select_env_vars {:#?}",
        environ_path, filter_env_vars, select_env_vars
    );

    let mut environ_file = tokio::fs::File::open(environ_path).await.map_err(|fail| {
        ResponseError::FileOperation(FileError {
            operation: "open".to_string(),
            raw_os_error: fail.raw_os_error(),
            kind: fail.kind().into(),
        })
    })?;

    let mut raw_env_vars = String::with_capacity(8192);

    // TODO: nginx doesn't play nice when we do this, it only returns a string that goes like
    // "nginx -g daemon off;".
    let read_amount = environ_file
        .read_to_string(&mut raw_env_vars)
        .await
        .map_err(|fail| {
            ResponseError::FileOperation(FileError {
                operation: "read_to_string".to_string(),
                raw_os_error: fail.raw_os_error(),
                kind: fail.kind().into(),
            })
        })?;
    debug!(
        "select_env_vars -> read {:#?} bytes with pure ENV_VARS {:#?}",
        read_amount, raw_env_vars
    );

    // TODO: These are env vars that should usually be ignored. Revisit this list if a user
    // ever asks for a way to NOT filter out these.
    let mut default_filter = HashSet::with_capacity(2);
    default_filter.insert("PATH".to_string());
    default_filter.insert("HOME".to_string());

    let env_vars = raw_env_vars
        // "DB=foo.db\0PORT=99\0HOST=\0PATH=/fake\0"
        .split_terminator(char::from(0))
        // ["DB=foo.db", "PORT=99", "HOST=", "PATH=/fake"]
        .map(|key_and_value| key_and_value.split_terminator('=').collect::<Vec<_>>())
        // [["DB", "foo.db"], ["PORT", "99"], ["HOST"], ["PATH", "/fake"]]
        .filter_map(
            |mut keys_and_values| match (keys_and_values.pop(), keys_and_values.pop()) {
                (Some(value), Some(key)) => Some((key.to_string(), value.to_string())),
                _ => None,
            },
        )
        .filter(|(key, _)| !default_filter.contains(key))
        // [("DB", "foo.db"), ("PORT", "99"), ("PATH", "/fake")]
        .filter(|(key, _)| !filter_env_vars.contains(key))
        // [("DB", "foo.db"), ("PORT", "99")]
        .filter(|(key, _)| {
            select_env_vars.is_empty()
                || select_env_vars.contains("*")
                || select_env_vars.contains(key)
        })
        // [("DB", "foo.db")]
        .collect::<HashMap<_, _>>();

    debug!("select_env_vars -> selected env vars found {:?}", env_vars);

    Ok(env_vars)
}

async fn handle_peer_messages(
    // TODO: Possibly refactor `state` out to be more "independent", and live in its own worker
    // thread.
    state: &mut State,
    sniffer_command_tx: mpsc::Sender<SnifferCommand>,
    peer_message: PeerMessage,
) -> Result<(), AgentError> {
    match peer_message.client_message {
        ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)) => {
            debug!(
                "ClientMessage::PortUnsubscribe -> peer id {:#?}, port {port:#?}",
                peer_message.peer_id
            );
            state
                .port_subscriptions
                .unsubscribe(peer_message.peer_id, port);
        }
        ClientMessage::Tcp(LayerTcp::PortSubscribe(port)) => {
            debug!(
                "ClientMessage::PortSubscribe -> peer id {:#?} asked to subscribe to {:#?}",
                peer_message.peer_id, port
            );
            state
                .port_subscriptions
                .subscribe(peer_message.peer_id, port);

            let ports = state.port_subscriptions.get_subscribed_topics();
            sniffer_command_tx
                .send(SnifferCommand::SetPorts(ports))
                .await?;
        }
        ClientMessage::Close => {
            debug!(
                "ClientMessage::Close -> peer id {:#?} sent close",
                &peer_message.peer_id
            );
            state.remove_peer(peer_message.peer_id);

            let ports = state.port_subscriptions.get_subscribed_topics();
            sniffer_command_tx
                .send(SnifferCommand::SetPorts(ports))
                .await?;
        }
        ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id)) => {
            debug!("ClientMessage::ConnectionUnsubscribe -> peer id {:#?} unsubscribe connection id {:#?}", &peer_message.peer_id, connection_id);
            state
                .connections_subscriptions
                .unsubscribe(peer_message.peer_id, connection_id);
        }
        ClientMessage::Ping => {
            // handled by peer
        }
        ClientMessage::FileRequest(_) => {
            // handled by the peer..
        }
        ClientMessage::GetEnvVarsRequest(_) => {
            // handled by peer
        }
    }

    Ok(())
}

struct PeerHandler {
    pub sender: Sender<PeerMessage>,
    id: AgentID,
    file_manager: FileManager,
    stream: Framed<TcpStream, DaemonCodec>,
    receiver: Receiver<DaemonMessage>,
    pid: Option<u64>,
}

impl AgentConnectionHandler {
    /// A loop that handles agent connection and state. Brekas upon receiver/sender drop.
    pub async fn start(
        id: AgentID,
        stream: TcpStream,
        pid: Option<u64>,
        sniffer_command_sender: Sender<SnifferCommand>,
    ) -> Result<(), AgentError> {
        let file_manager = FileManager::new(pid);
        let stream = actix_codec::Framed::new(stream, DaemonCodec::new());
        let (tcp_receiver, tcp_sender) = mpsc::channel(CHANNEL_SIZE);
        let tcp_sniffer_api = TCPSnifferAPI::new(id, sniffer_command_sender, tcp_receiver)
            .enable(tcp_sender)
            .await?;
        let mut agent_handler = AgentConnectionHandler {
            id,
            file_manager,
            stream,
            receiver,
            pid,
            tcp_sniffer_api,
        };
        agent_handler.handle_loop().await?;
        Ok(())
    }

    async fn handle_loop(&mut self) -> Result<(), AgentError> {
        let mut running = true;
        while running {
            select! {
                message = self.stream.next() => {
                    if let Some(message) = message {
                        running = self.handle_agent_message(message?).await?;
                    } else {
                        debug!("Agent {} disconnected", self.id);
                            break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle incoming messages from the agent. Returns False if the agent disconnected.
    async fn handle_agent_message(&mut self, message: ClientMessage) -> Result<bool, AgentError> {
        debug!("agent_handler -> client sent message {:?}", message);
        match message {
            ClientMessage::FileRequest(req) => {
                let response = self.file_manager.handle_message(req)?;
                self.stream
                    .send(DaemonMessage::FileResponse(response))
                    .await
                    .map_err(From::from)?
            }
            ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
                env_vars_select,
            }) => {
                debug!(
                    "ClientMessage::GetEnvVarsRequest peer id {:?} filter {:?} select {:?}",
                    self.id, env_vars_filter, env_vars_select
                );

                let pid = self
                    .pid
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "self".to_string());
                let environ_path = PathBuf::from("/proc").join(pid).join("environ");

                let env_vars_result =
                    select_env_vars(environ_path, env_vars_filter, env_vars_select).await;

                self.stream
                    .send(DaemonMessage::GetEnvVarsResponse(env_vars_result))
                    .await
                    .map_err(From::from)
            }
            },
            ClientMessage::Ping => {
                self.stream.send(DaemonMessage::Pong).await.map_err(From::from)
            },
            ClientMessage::Ping => self
                .stream
                .send(DaemonMessage::Pong)
                .await
                .map_err(From::from)?,
            ClientMessage::Tcp(message) => self.handle_agent_tcp(message).await?,
            ClientMessage::Close => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn handle_agent_tcp(&mut self, message: LayerTcp) -> Result<(), AgentError> {
        match message {
            LayerTcp::PortSubscribe(port) => self.tcp_sniffer_api.subscribe(port).await,
            LayerTcp::ConnectionUnsubscribe(connection_id) => {
                self.tcp_sniffer_api.unsubscribe(connection_id).await
            }
            LayerTcp::PortUnsubscribe(port) => self.tcp_sniffer_api.unsubscribe_port(port).await,
        }
    }

async fn start_agent() -> Result<(), AgentError> {
    let args = parse_args();

    let listener = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::new(0, 0, 0, 0),
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

    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);

    let sniffer_task = tokio::spawn(TCPConnectionSniffer::start(
        sniffer_command_rx,
        pid,
        args.interface,
    ));

    let agents = FuturesUnordered::new();
    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                debug!("start -> Connection accepted from {:?}", addr);

                if let Some(agent_id) = state.generate_id() {

                    state.agents.insert(agent_id);

                    let agent = tokio::spawn(async move {
                        match AgentConnectionHandler::start(agent_id, stream, pid, sniffer_command_tx.clone()).await {
                            Ok(_) => {
                                debug!("AgentConnectionHandler::start -> Agent {} disconnected", agent_id);
                            }
                            Err(e) => {
                                error!("AgentConnectionHandler::start -> Agent {} disconnected with error: {}", agent_id, e);
                            }
                        }
                        agent_id

                    });
                    agents.push(agent);
                }
                else {
                    error!("start_agent -> Ran out of connections, dropping new connection");
                }

            },
            agent = agents.select_next_some() => {
                let agent_id = agent.await?;
                state.remove_agent(agent_id);
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(args.communication_timeout.into())) => {
                if state.agents.is_empty() {
                    debug!("start_agent -> main thread timeout, no agents connected");
                    break;
                }
            }
        }
    }

    debug!("start_agent -> shutting down start");
    if !sniffer_command_tx.is_closed() {
        sniffer_command_tx.send(SnifferCommand::Close).await?;
    };

    // To make tasks stop (need to add drain..)
    drop(sniffer_command_tx);
    drop(sniffer_output_rx);

    tokio::time::timeout(std::time::Duration::from_secs(10), packet_task).await???;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), AgentError> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
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
