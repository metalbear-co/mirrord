#![feature(result_option_inspect)]
#![feature(hash_drain_filter)]

use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use actix_codec::Framed;
use error::AgentError;
use file::FileManager;
use futures::{
    stream::{FuturesUnordered, StreamExt},
    SinkExt,
};
use mirrord_protocol::{
    tcp::LayerTcp, AddrInfoHint, AddrInfoInternal, ClientMessage, DaemonCodec, DaemonMessage,
    GetAddrInfoRequest, GetEnvVarsRequest, RemoteResult, ResponseError,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};
use tracing_subscriber::prelude::*;

mod cli;
mod error;
mod file;
mod runtime;
mod sniffer;
mod util;

use cli::parse_args;
use sniffer::{SnifferCommand, TCPConnectionSniffer, TCPSnifferAPI};
use util::{ClientID, IndexAllocator};

use crate::runtime::get_container_pid;

trait AddrInfoHintExt {
    fn into_lookup(self) -> dns_lookup::AddrInfoHints;
}

impl AddrInfoHintExt for AddrInfoHint {
    fn into_lookup(self) -> dns_lookup::AddrInfoHints {
        dns_lookup::AddrInfoHints {
            socktype: self.ai_socktype,
            protocol: self.ai_protocol,
            address: self.ai_family,
            flags: self.ai_flags,
        }
    }
}

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

/// Helper function that loads the process' environment variables, and selects only those that were
/// requested from `mirrord-layer` (ignores vars specified in `filter_env_vars`).
///
/// Returns an error if none of the requested environment variables were found.
async fn select_env_vars(
    environ_path: PathBuf,
    filter_env_vars: HashSet<String>,
    select_env_vars: HashSet<String>,
) -> RemoteResult<HashMap<String, String>> {
    trace!(
        "select_env_vars -> environ_path {:#?} filter_env_vars {:#?} select_env_vars {:#?}",
        environ_path,
        filter_env_vars,
        select_env_vars
    );

    let mut environ_file = tokio::fs::File::open(environ_path).await?;

    let mut raw_env_vars = String::with_capacity(8192);

    // TODO: nginx doesn't play nice when we do this, it only returns a string that goes like
    // "nginx -g daemon off;".
    let _read_amount = environ_file.read_to_string(&mut raw_env_vars).await?;

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

    Ok(env_vars)
}

/// Handles the `getaddrinfo` call from mirrord-layer.
fn get_addr_info(request: GetAddrInfoRequest) -> RemoteResult<Vec<AddrInfoInternal>> {
    trace!("get_addr_info -> request {:#?}", request);

    let GetAddrInfoRequest {
        node,
        service,
        hints,
    } = request;

    dns_lookup::getaddrinfo(
        node.as_deref(),
        service.as_deref(),
        hints.map(|h| h.into_lookup()),
    )
    .map(|addrinfo_iter| {
        addrinfo_iter
            .map(|result| {
                // Each element in the iterator is actually a `Result<AddrInfo, E>`, so
                // we have to `map` individually, then convert to one of our errors.
                result.map(Into::into).map_err(From::from)
            })
            // Now we can flatten and transpose the whole thing into this.
            .collect::<Result<Vec<AddrInfoInternal>, _>>()
    })
    .map_err(|fail| ResponseError::from(std::io::Error::from(fail)))
    // Stable rust equivalent to `Result::flatten`.
    .and_then(std::convert::identity)
}

struct ClientConnectionHandler {
    id: ClientID,
    file_manager: FileManager,
    stream: Framed<TcpStream, DaemonCodec>,
    pid: Option<u64>,
    tcp_sniffer_api: TCPSnifferAPI,
}

impl ClientConnectionHandler {
    /// A loop that handles client connection and state. Brekas upon receiver/sender drop.
    pub async fn start(
        id: ClientID,
        stream: TcpStream,
        pid: Option<u64>,
        sniffer_command_sender: Sender<SnifferCommand>,
        cancel_token: CancellationToken,
    ) -> Result<(), AgentError> {
        let file_manager = FileManager::new(pid);
        let stream = actix_codec::Framed::new(stream, DaemonCodec::new());

        let (tcp_sender, tcp_receiver) = mpsc::channel(CHANNEL_SIZE);
        let tcp_sniffer_api =
            TCPSnifferAPI::new(id, sniffer_command_sender, tcp_receiver, tcp_sender).await?;

        let mut client_handler = ClientConnectionHandler {
            id,
            file_manager,
            stream,
            pid,
            tcp_sniffer_api,
        };

        client_handler.handle_loop(cancel_token).await?;

        Ok(())
    }

    async fn handle_loop(&mut self, token: CancellationToken) -> Result<(), AgentError> {
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
                        self.stream.send(DaemonMessage::Tcp(message)).await?;
                    } else {
                        error!("tcp sniffer stopped?");
                        break;
                    }
                }
                _ = token.cancelled() => {
                    break;
                }
            }
        }
        debug!("client closing");
        Ok(())
    }

    /// Handle incoming messages from the client. Returns False if the client disconnected.
    async fn handle_client_message(&mut self, message: ClientMessage) -> Result<bool, AgentError> {
        debug!("client_handler -> client sent message {:?}", message);
        match message {
            ClientMessage::FileRequest(req) => {
                let response = self.file_manager.handle_message(req)?;
                self.stream.send(DaemonMessage::File(response)).await?
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
                    select_env_vars(environ_path, env_vars_filter, env_vars_select).await;

                self.stream
                    .send(DaemonMessage::GetEnvVarsResponse(env_vars_result))
                    .await?
            }
            ClientMessage::GetAddrInfoRequest(request) => {
                let response = get_addr_info(request);

                trace!("GetAddrInfoRequest -> response {:#?}", response);

                self.stream
                    .send(DaemonMessage::GetAddrInfoResponse(response))
                    .await?
            }
            ClientMessage::Ping => self.stream.send(DaemonMessage::Pong).await?,
            ClientMessage::Tcp(message) => self.handle_client_tcp(message).await?,
            ClientMessage::Close => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn handle_client_tcp(&mut self, message: LayerTcp) -> Result<(), AgentError> {
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
    let cancellation_token = CancellationToken::new();
    // Cancel all other tasks on exit
    let cancel_guard = cancellation_token.clone().drop_guard();
    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);

    let sniffer_task = tokio::spawn(TCPConnectionSniffer::start(
        sniffer_command_rx,
        pid,
        args.interface,
        cancellation_token.clone(),
    ));

    let mut clients = FuturesUnordered::new();
    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                debug!("start -> Connection accepted from {:?}", addr);

                if let Some(client_id) = state.generate_id() {

                    state.clients.insert(client_id);
                    let sniffer_command_tx = sniffer_command_tx.clone();
                    let cancellation_token = cancellation_token.clone();
                    let client = tokio::spawn(async move {
                        match ClientConnectionHandler::start(client_id, stream, pid, sniffer_command_tx, cancellation_token).await {
                            Ok(_) => {
                                debug!("ClientConnectionHandler::start -> Client {} disconnected", client_id);
                            }
                            Err(e) => {
                                error!("ClientConnectionHandler::start -> Client {} disconnected with error: {}", client_id, e);
                            }
                        }
                        client_id

                    });
                    clients.push(client);
                }
                else {
                    error!("start_client -> Ran out of connections, dropping new connection");
                }

            },
            Some(client) = clients.next() => {
                let client_id = client?;
                state.remove_client(client_id);
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(args.communication_timeout.into())) => {
                if state.clients.is_empty() {
                    debug!("start_agent -> main thread timeout, no clients connected");
                    break;
                }
            }
        }
    }

    debug!("start_agent -> shutting down start");
    drop(cancel_guard);
    sniffer_task.await??;

    debug!("shutdown done");
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
