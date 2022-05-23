use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    fs::File,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use actix_codec::Framed;
use anyhow::Result;
use futures::SinkExt;
use mirrord_protocol::{
    ClientMessage, ConnectionID, DaemonCodec, DaemonMessage, OpenFileResponse, Port,
    ReadFileRequest, ReadFileResponse, SeekFileRequest, SeekFileResponse,
};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self},
};
use tokio_stream::StreamExt;
use tracing::{debug, error, info, warn};

mod cli;
mod runtime;
mod sniffer;
mod util;

use cli::parse_args;
use sniffer::{packet_worker, SnifferCommand, SnifferOutput};
use util::{IndexAllocator, Subscriptions};

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

#[derive(Debug, Default)]
struct FileManager {
    pub open_files: HashMap<i32, File>,
}

impl FileManager {
    // TODO(alex) [mid] 2022-05-19: Need to do a better job handling errors here (and in `read`)?
    // Ideally we would send the error back in the `Response`, to be handled by the mirrod-layer
    // side, and converted into some `libc::X` value.
    pub(crate) fn open(&mut self, path: PathBuf) -> Result<i32> {
        debug!("FileManager::open -> Trying to open file {path:#?}.");

        // TODO(alex) [low] 2022-05-18: To avoid opening the same file multiple times, we could
        // search `FileManager::open_files` for a path that matches the one passed as a parameter.
        let file = std::fs::File::open(path)?;
        let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);

        self.open_files.insert(fd, file);

        Ok(fd)
    }

    pub(crate) fn read(&self, fd: i32, buffer_size: usize) -> Result<ReadFileResponse> {
        let mut file = self.open_files.get(&fd).unwrap();

        debug!("FileManager::read -> Trying to read file {file:#?}, with count {buffer_size:#?}");

        let mut buffer = vec![0; buffer_size];
        let read_amount = file.read(&mut buffer)?;

        let response = ReadFileResponse {
            bytes: buffer,
            read_amount,
        };

        Ok(response)
    }

    pub(crate) fn seek(&self, fd: i32, seek_from: SeekFrom) -> Result<SeekFileResponse> {
        let mut file = self.open_files.get(&fd).unwrap();

        debug!("FileManager::seek -> Trying to seek file {file:#?}, with seek {seek_from:#?}");

        let result_offset = file.seek(seek_from)?;

        let response = SeekFileResponse { result_offset };
        Ok(response)
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
struct PeerMessage {
    client_message: ClientMessage,
    peer_id: PeerID,
}

async fn handle_peer_messages(
    daemon_messages_tx: mpsc::Sender<PeerMessage>,
    daemon_stream: &mut Framed<TcpStream, DaemonCodec>,
    peer_id: PeerID,
    message: Option<Result<ClientMessage, std::io::Error>>,
    file_manager: &mut FileManager,
) -> Result<()> {
    if let Some(message) = message {
        let message = PeerMessage {
            client_message: message.unwrap(),
            peer_id,
        };
        debug!("handle_peer_message -> client sent message {:?}", &message);

        match message.client_message {
            ClientMessage::OpenFileRequest(path) => {
                debug!(
                    "handle_peer_messages -> peer id {:?} asked to open file {path:?}",
                    message.peer_id
                );

                let file_fd = file_manager.open(path)?;
                let open_file_response =
                    DaemonMessage::OpenFileResponse(OpenFileResponse { fd: file_fd });

                daemon_stream.send(open_file_response).await?;
            }
            ClientMessage::ReadFileRequest(ReadFileRequest { fd, buffer_size }) => {
                debug!(
                    "handle_peer_messages -> peer id {:?} asked to read file {fd:#?}",
                    message.peer_id
                );

                let read_file_response = file_manager.read(fd, buffer_size)?;
                debug!("handle_peer_messages -> file read operation was successful.");

                let send_result = daemon_stream
                    .send(DaemonMessage::ReadFileResponse(read_file_response))
                    .await;
                debug!("handle_peer_messages -> `send_result` {send_result:#?}.");
            }
            ClientMessage::SeekFileRequest(SeekFileRequest { fd, seek_from }) => {
                debug!(
                    "handle_peer_messages -> peer id {:?} asked to seek file {fd:#?}",
                    message.peer_id
                );

                let seek_file_response = file_manager.seek(fd, seek_from.into())?;
                debug!("handle_peer_messages -> file seek operation was successful.");

                let send_result = daemon_stream
                    .send(DaemonMessage::SeekFileResponse(seek_file_response))
                    .await;
                debug!("handle_peer_messages -> `send_result` {send_result:#?}.");
            }
            ClientMessage::Close => daemon_messages_tx.send(message).await?,
            ClientMessage::PortSubscribe(_) => daemon_messages_tx.send(message).await?,
            ClientMessage::ConnectionUnsubscribe(_) => daemon_messages_tx.send(message).await?,
        }

        Ok(())
    } else {
        // TODO(alex) [mid] 2022-05-19: Figure out what should happen here.
        warn!(
            "handle_peer_messages -> Have no idea yet what is supposed to happen here {:#?}.",
            message
        );
        todo!()
    }
}

async fn peer_handler(
    mut daemon_messages_rx: mpsc::Receiver<DaemonMessage>,
    daemon_messages_tx: mpsc::Sender<PeerMessage>,
    stream: TcpStream,
    peer_id: PeerID,
) -> Result<()> {
    let mut daemon_stream = actix_codec::Framed::new(stream, DaemonCodec::new());

    let mut file_manager = FileManager::default();

    loop {
        select! {
            message = daemon_stream.next() => {
                handle_peer_messages(daemon_messages_tx.clone(), &mut daemon_stream, peer_id, message, &mut file_manager).await?;
            },
            message = daemon_messages_rx.recv() => {
                match message {
                    Some(message) => {
                        debug!("send message to client {:?}", &message);
                        daemon_stream.send(message).await?;
                    }
                    None => break
                }

            }
        }
    }

    daemon_messages_tx
        .send(PeerMessage {
            client_message: ClientMessage::Close,
            peer_id,
        })
        .await?;

    Ok(())
}

async fn start() -> Result<()> {
    let args = parse_args();
    debug!("mirrord-agent starting with args {:?}", args);

    let listener = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::new(0, 0, 0, 0),
        args.communicate_port,
    ))
    .await?;

    let mut state = State::new();
    let (peers_tx, mut peers_rx) = mpsc::channel::<PeerMessage>(1000);
    let (packet_sniffer_tx, mut packet_sniffer_rx) = mpsc::channel::<SnifferOutput>(1000);
    let (packet_command_tx, packet_command_rx) = mpsc::channel::<SnifferCommand>(1000);
    // We use tokio spawn so it'll create another thread.
    let packet_task = tokio::spawn(packet_worker(
        packet_sniffer_tx,
        packet_command_rx,
        args.interface.clone(),
        args.container_id.clone(),
    ));
    loop {
        select! {
            Ok((stream, addr)) = listener.accept() => {
                debug!("Connection accepeted from {:?}", addr);
                if let Some(id) = state.generate_id() {
                    let (tx, rx) = mpsc::channel::<DaemonMessage>(1000);
                    state.peers.insert(Peer::new(id, tx));
                    let worker_tx = peers_tx.clone();
                    tokio::spawn(async move {
                        match peer_handler(rx, worker_tx, stream, id).await {
                            Ok(()) => {debug!("Peer closed")},
                            Err(err) => {error!("Peer encountered error {}", err.to_string());}
                        };
                    });
                }
                else {
                    error!("ran out of connections, dropping new connection");
                }

            },
            Some(message) = peers_rx.recv() => {
                match message.client_message {
                    ClientMessage::PortSubscribe(ports) => {
                        debug!("peer id {:?} asked to subscribe to {:?}", message.peer_id, ports);
                        state.port_subscriptions.subscribe_many(message.peer_id, ports);
                        let ports = state.port_subscriptions.get_subscribed_topics();
                        packet_command_tx.send(SnifferCommand::SetPorts(ports)).await?;
                    }
                    ClientMessage::Close => {
                        debug!("peer id {:?} sent close", &message.peer_id);
                        state.remove_peer(message.peer_id);
                        let ports = state.port_subscriptions.get_subscribed_topics();
                        packet_command_tx.send(SnifferCommand::SetPorts(ports)).await?;
                    },
                    ClientMessage::ConnectionUnsubscribe(connection_id) => {
                        state.connections_subscriptions.unsubscribe(message.peer_id, connection_id);
                    }
                    ClientMessage::OpenFileRequest(_) => {
                        // TODO(alex) [low] 2022-05-19: These are unreachable, as they're handled
                        // by `peer_handler(...)`.
                        //
                        // @aviramha suggested creating a different `enum` for these messages.

                        // NOTE(alex): `peers_rx` never receives this type of message, as it is
                        // handled in `peer_handler`.
                        unreachable!();
                    }
                    ClientMessage::ReadFileRequest(_) => {
                        // NOTE(alex): Same as the `OpenFileRequest` case.
                        unreachable!();
                    }
                    ClientMessage::SeekFileRequest(_) => {
                        // NOTE(alex): Same as the `OpenFileRequest` case.
                        unreachable!();
                    }
                }
            },
            message = packet_sniffer_rx.recv() => {
                match message {
                    Some(message) => {
                        match message {
                            SnifferOutput::NewTCPConnection(conn) => {
                                let peer_ids = state.port_subscriptions.get_topic_subscribers(conn.destination_port);
                                for peer_id in peer_ids {
                                    state.connections_subscriptions.subscribe(peer_id, conn.connection_id);
                                    if let Some(peer) = state.peers.get(&peer_id) {
                                        match peer.channel.send(DaemonMessage::NewTCPConnection(conn.clone())).await {
                                            Ok(_) => {},
                                            Err(err) => {
                                                error!("error sending message {:?}", err);
                                            }
                                        }
                                    }
                                }
                                debug!("new tcp connection {:?}", conn);
                            },
                            SnifferOutput::TCPClose(close) => {
                                let peer_ids = state.connections_subscriptions.get_topic_subscribers(close.connection_id);
                                for peer_id in peer_ids {
                                    if let Some(peer) = state.peers.get(&peer_id) {
                                        match peer.channel.send(DaemonMessage::TCPClose(close.clone())).await {
                                            Ok(_) => {},
                                            Err(err) => {
                                                error!("error sending message {:?}", err);
                                            }
                                        }
                                    }
                                }
                                state.connections_subscriptions.remove_topic(close.connection_id);
                                debug!("tcp close {:?}", close);
                            },
                            SnifferOutput::TCPData(data) => {
                                let peer_ids = state.connections_subscriptions.get_topic_subscribers(data.connection_id);
                                for peer_id in peer_ids {
                                    if let Some(peer) = state.peers.get(&peer_id) {
                                        match peer.channel.send(DaemonMessage::TCPData(data.clone())).await {
                                            Ok(_) => {},
                                            Err(err) => {
                                                error!("error sending message {:?}", err);
                                            }
                                        }
                                    }
                                }
                                debug!("new data {:?}", data);
                            },
                        }
                    }
                    None => {
                        info!("sniffer died, exiting");
                        break;
                    }
                }

            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(args.communication_timeout.into())) => {
                if state.peers.is_empty() {
                    debug!("main thread timeout, no peers connected");
                    break;
                }
            }
        }
    }
    debug!("shutting down start");
    if !packet_command_tx.is_closed() {
        packet_command_tx.send(SnifferCommand::Close).await?;
    };
    drop(packet_command_tx);
    drop(packet_sniffer_rx);
    tokio::time::timeout(std::time::Duration::from_secs(10), packet_task).await???;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    match start().await {
        Ok(_) => {
            info!("Exiting successfuly")
        }
        Err(err) => {
            error!("error occured: {:?}", err.to_string())
        }
    }
    Ok(())
}
