#![feature(result_option_inspect)]
#![feature(never_type)]

use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    net::{Ipv4Addr, SocketAddrV4},
};

use error::AgentError;
use futures::SinkExt;
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp},
    ClientMessage, ConnectionID, DaemonCodec, DaemonMessage, FileRequest, FileResponse, Port,
};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self},
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
use sniffer::{packet_worker, SnifferCommand, SnifferOutput};
use util::{IndexAllocator, Subscriptions};

use crate::file::file_worker;

type PeerID = u32;

#[derive(Debug)]
struct Peer {
    id: PeerID,
    channel: mpsc::Sender<DaemonMessage>,
}

impl Peer {
    pub fn new(id: PeerID, channel: mpsc::Sender<DaemonMessage>) -> Peer {
        Peer { id, channel }
    }
}
impl Eq for Peer {}

impl PartialEq for Peer {
    fn eq(&self, other: &Peer) -> bool {
        self.id == other.id
    }
}

impl Hash for Peer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Borrow<PeerID> for Peer {
    fn borrow(&self) -> &PeerID {
        &self.id
    }
}

#[derive(Debug)]
struct State {
    pub peers: HashSet<Peer>,
    index_allocator: IndexAllocator<PeerID>,
    pub port_subscriptions: Subscriptions<Port, PeerID>,
    pub connections_subscriptions: Subscriptions<ConnectionID, PeerID>,
}

impl State {
    pub fn new() -> State {
        State {
            peers: HashSet::new(),
            index_allocator: IndexAllocator::new(),
            port_subscriptions: Subscriptions::new(),
            connections_subscriptions: Subscriptions::new(),
        }
    }

    pub fn generate_id(&mut self) -> Option<PeerID> {
        self.index_allocator.next_index()
    }

    pub fn remove_peer(&mut self, peer_id: PeerID) {
        self.peers.remove(&peer_id);
        self.port_subscriptions.remove_client(peer_id);
        self.connections_subscriptions.remove_client(peer_id);
        self.index_allocator.free_index(peer_id)
    }
}

#[derive(Debug)]
pub struct PeerMessage {
    client_message: ClientMessage,
    peer_id: PeerID,
}

async fn handle_peer_messages(
    // TODO: Possibly refactor `state` out to be more "independent", and live in its own worker
    // thread.
    state: &mut State,
    sniffer_command_tx: mpsc::Sender<SnifferCommand>,
    file_request_tx: mpsc::Sender<(PeerID, FileRequest)>,
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
            trace!("peer id {:?} sent ping", &peer_message.peer_id);
            let peer = state.peers.get(&peer_message.peer_id).unwrap();
            peer.channel.send(DaemonMessage::Pong).await?;
        }
        ClientMessage::FileRequest(file_request) => {
            debug!(
                "ClientMessage::FileRequest peer id {:?} requesting {:?}",
                peer_message.peer_id, file_request
            );

            file_request_tx
                .send((peer_message.peer_id, file_request))
                .await?;
        }
    }

    Ok(())
}

async fn peer_handler(
    mut daemon_messages_rx: mpsc::Receiver<DaemonMessage>,
    peer_messages_tx: mpsc::Sender<PeerMessage>,
    stream: TcpStream,
    peer_id: PeerID,
) -> Result<(), AgentError> {
    let mut daemon_stream = actix_codec::Framed::new(stream, DaemonCodec::new());

    loop {
        select! {
            message = daemon_stream.next() => {
                debug!("peer_handler -> daemon_stream.next received a message {:?}", message);

                match message {
                    Some(message) => {
                        let message = PeerMessage {
                            client_message: message?,
                            peer_id
                        };

                        debug!("peer_handler -> client sent message {:?}", message);
                        peer_messages_tx.send(message).await?;
                    }
                    None => break
                }
            },
            message = daemon_messages_rx.recv() => {
                debug!("peer_handler -> daemon_messages_rx.recv received a message {:?}", message);

                match message {
                    Some(message) => {
                        debug!("peer_handler -> send message to client");
                        daemon_stream.send(message).await?;
                    }
                    None => ()
                }

            }
        }
    }

    peer_messages_tx
        .send(PeerMessage {
            client_message: ClientMessage::Close,
            peer_id,
        })
        .await?;

    Ok(())
}

async fn start_agent() -> Result<(), AgentError> {
    let args = parse_args();

    let listener = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::new(0, 0, 0, 0),
        args.communicate_port,
    ))
    .await?;

    let mut state = State::new();

    let (peer_messages_tx, mut peer_messages_rx) = mpsc::channel::<PeerMessage>(1000);
    let (sniffer_output_tx, mut sniffer_output_rx) = mpsc::channel::<SnifferOutput>(1000);
    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);

    // We use tokio spawn so it'll create another thread (default tokio runtime configuration).
    let packet_task = tokio::spawn(packet_worker(
        sniffer_output_tx,
        sniffer_command_rx,
        args.interface.clone(),
        args.container_id.clone(),
        args.container_runtime.clone(),
    ));

    let (file_request_tx, file_request_rx) = mpsc::channel::<(PeerID, FileRequest)>(1000);
    let (file_response_tx, mut file_response_rx) = mpsc::channel::<(PeerID, FileResponse)>(1000);
    // Create a task to handle file operations, similar to sniffer.
    let file_task = tokio::spawn(file_worker(
        file_request_rx,
        file_response_tx,
        args.container_id.clone(),
        args.container_runtime.clone(),
    ));

    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                debug!("start -> Connection accepted from {:?}", addr);

                if let Some(peer_id) = state.generate_id() {
                    let (daemon_message_tx, daemon_message_rx) = mpsc::channel::<DaemonMessage>(1000);
                    let peer_messages_tx = peer_messages_tx.clone();

                    state.peers.insert(Peer::new(peer_id, daemon_message_tx));

                    tokio::spawn(async move {
                        let _ = peer_handler(daemon_message_rx, peer_messages_tx, stream, peer_id)
                            .await
                            .inspect(|_| debug!("start_agent -> Peer {:#?} closed", peer_id))
                            .inspect_err(|fail| error!("start_agent -> Peer {:#?} failed with {:#?}", peer_id, fail));
                    });
                }
                else {
                    error!("start_agent -> Ran out of connections, dropping new connection");
                }

            },
            Some(peer_message) = peer_messages_rx.recv() => {
                handle_peer_messages(&mut state, sniffer_command_tx.clone(), file_request_tx.clone(), peer_message).await?;

            },
            Some((peer_id, file_response)) = file_response_rx.recv() => {
                if let Some(peer) = state.peers.get(&peer_id) {
                    peer.channel.send(DaemonMessage::FileResponse(file_response))
                        .await
                        .inspect_err(|fail| error!("Failed sending message {:?} to peer {:?}", fail, peer_id))?;
                }
            }
            sniffer_output = sniffer_output_rx.recv() => {
                match sniffer_output {
                    Some(sniffer_output) => {
                        match sniffer_output {
                            SnifferOutput::NewTcpConnection(new_connection) => {
                                debug!("SnifferOutput::NewTcpConnection -> connection {:#?}", new_connection);
                                let peer_ids = state.port_subscriptions.get_topic_subscribers(new_connection.destination_port);

                                for peer_id in peer_ids {
                                    state.connections_subscriptions.subscribe(peer_id, new_connection.connection_id);

                                    if let Some(peer) = state.peers.get(&peer_id) {
                                        match peer.channel.send(DaemonMessage::Tcp(DaemonTcp::NewConnection(new_connection.clone()))).await {
                                            Ok(_) => {},
                                            Err(err) => {
                                                error!("error sending message {:?}", err);
                                            }
                                        }
                                    }
                                }
                            },
                            SnifferOutput::TcpClose(close) => {
                                debug!("SnifferOutput::TcpClose -> close {:#?}", close);
                                let peer_ids = state.connections_subscriptions.get_topic_subscribers(close.connection_id);

                                for peer_id in peer_ids {
                                    if let Some(peer) = state.peers.get(&peer_id) {
                                        match peer.channel.send(DaemonMessage::Tcp(DaemonTcp::Close(close.clone()))).await {
                                            Ok(_) => {},
                                            Err(err) => {
                                                error!("error sending message {:?}", err);
                                            }
                                        }
                                    }
                                }
                                state.connections_subscriptions.remove_topic(close.connection_id);
                            },
                            SnifferOutput::TcpData(data) => {
                                debug!("SnifferOutput::TcpData -> data");
                                let peer_ids = state.connections_subscriptions.get_topic_subscribers(data.connection_id);

                                for peer_id in peer_ids {
                                    if let Some(peer) = state.peers.get(&peer_id) {
                                        match peer.channel.send(DaemonMessage::Tcp(DaemonTcp::Data(data.clone()))).await {
                                            Ok(_) => {},
                                            Err(err) => {
                                                error!("error sending message {:?}", err);
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                    None => {
                        info!("statr_agent -> None in SnifferOutput, exiting");
                        break;
                    }
                }
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(args.communication_timeout.into())) => {
                if state.peers.is_empty() {
                    debug!("start_agent -> main thread timeout, no peers connected");
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
    drop(file_request_tx);
    drop(file_response_rx);

    tokio::time::timeout(std::time::Duration::from_secs(10), packet_task).await???;
    tokio::time::timeout(std::time::Duration::from_secs(10), file_task).await???;

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
