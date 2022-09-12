use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    thread,
};

use futures::TryFutureExt;
use mirrord_protocol::{
    outgoing::{tcp::*, *},
    ConnectionId, ResponseError,
};
use streammap_ext::StreamMap;
use tokio::{
    io::AsyncWriteExt,
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpSocket,
    },
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::{debug, trace, warn};

use crate::{
    error::AgentError,
    runtime::set_namespace,
    util::{run_thread, IndexAllocator},
};

pub(crate) mod udp;

type Layer = LayerTcpOutgoing;
type Daemon = DaemonTcpOutgoing;

/// Handles (briefly) the `TcpOutgoingRequest` and `TcpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct TcpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<Result<(), AgentError>>,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<Layer>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Daemon>,
}

impl TcpOutgoingApi {
    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let task = run_thread(Self::interceptor_task(pid, layer_rx, daemon_tx));

        Self {
            _task: task,
            layer_tx,
            daemon_rx,
        }
    }

    /// Does the actual work for `Request`s and prepares the `Responses:
    async fn interceptor_task(
        pid: Option<u64>,
        mut layer_rx: Receiver<Layer>,
        daemon_tx: Sender<Daemon>,
    ) -> Result<(), AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace)?;
        }

        let mut allocator = IndexAllocator::default();

        // TODO: Right now we're manually keeping these 2 maps in sync (aviram suggested using
        // `Weak` for `writers`).
        let mut writers: HashMap<ConnectionId, OwnedWriteHalf> = HashMap::default();
        let mut readers: StreamMap<ConnectionId, ReaderStream<OwnedReadHalf>> =
            StreamMap::default();

        loop {
            select! {
                biased;

                // [layer] -> [agent]
                Some(layer_message) = layer_rx.recv() => {
                    trace!("interceptor_task -> layer_message {:?}", layer_message);

                    match layer_message {
                        // [user] -> [layer] -> [agent] -> [layer]
                        // `user` is asking us to connect to some remote host.
                        LayerTcpOutgoing::Connect(LayerConnect { remote_address }) => {
                            let (mirror_address, socket) = match remote_address {
                                std::net::SocketAddr::V4(_) => {
                                    let mirror_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
                                    (mirror_address, TcpSocket::new_v4())
                                }
                                std::net::SocketAddr::V6(_) => {
                                    let mirror_address = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
                                    (mirror_address, TcpSocket::new_v6())
                                }
                            };

                            debug!("mirror_address {:#?} and socket {:#?}", mirror_address, socket);

                            // TODO(alex) [high] 2022-09-12: Switch this back to using `TcpStream::connect`
                            // directly (instead of `TcpSocket`).
                            //
                            // We're still blocking on `socket.connect`. Try disabling the `outgoing_traffic`
                            // feature and running the go sample (just pass it `no-outgoing`, or whatever the
                            // proper name for disable is) to trace what should happen. Both with DNS on, and
                            // DNS off.
                            let daemon_connect = async move { socket }
                                .and_then(|socket| async move {
                                    debug!("created socket {:#?}", socket);

                                    socket.bind(mirror_address).map(|_| socket)
                                })
                                .and_then(|socket| async move {
                                    debug!("bound socket {:#?}", socket);

                                    socket.connect(remote_address).await
                                })
                                .await
                                .and_then(|remote_stream| {
                                    let connection_id = allocator
                                        .next_index()
                                        .ok_or_else(|| ResponseError::AllocationFailure("interceptor_task".to_string()))
                                        .unwrap() as ConnectionId;

                                    // Split the `remote_stream` so we can keep reading
                                    // and writing from multiple hosts without blocking.
                                    let (read_half, write_half) = remote_stream.into_split();
                                    writers.insert(connection_id, write_half);
                                    readers.insert(connection_id, ReaderStream::new(read_half));

                                    Ok(DaemonConnect { connection_id, remote_address } )
                                })
                                .map_err(From::from);


                            let daemon_message = DaemonTcpOutgoing::Connect(daemon_connect);
                            daemon_tx.send(daemon_message).await?
                        }
                        // [user] -> [layer] -> [agent] -> [remote]
                        // `user` wrote some message to the remote host.
                        LayerTcpOutgoing::Write(LayerWrite {
                            connection_id,
                            bytes,
                        }) => {
                            let daemon_write = match writers
                                .get_mut(&connection_id)
                                .ok_or(ResponseError::NotFound(connection_id as usize))
                            {
                                Ok(writer) => writer
                                    .write_all(&bytes)
                                    .await
                                    .map_err(ResponseError::from),
                                Err(fail) => Err(fail),
                            };

                            if let Err(fail) = daemon_write {
                                warn!("LayerTcpOutgoing::Write -> Failed with {:#?}", fail);
                                writers.remove(&connection_id);
                                readers.remove(&connection_id);

                                let daemon_message = DaemonTcpOutgoing::Close(connection_id);
                                daemon_tx.send(daemon_message).await?
                            }
                        }
                        // [layer] -> [agent]
                        // `layer` closed their interceptor stream.
                        LayerTcpOutgoing::Close(LayerClose { ref connection_id }) => {
                            writers.remove(connection_id);
                            readers.remove(connection_id);
                        }
                    }
                }

                // [remote] -> [agent] -> [layer] -> [user]
                // Read the data from one of the connected remote hosts, and forward the result back
                // to the `user`.
                Some((connection_id, remote_read)) = readers.next() => {
                    trace!("interceptor_task -> read connection_id {:#?}", connection_id);

                    match remote_read {
                        Some(read) => {
                            let daemon_read = read
                                .map_err(ResponseError::from)
                                .map(|bytes| DaemonRead { connection_id, bytes: bytes.to_vec() });

                            let daemon_message = DaemonTcpOutgoing::Read(daemon_read);
                            daemon_tx.send(daemon_message).await?
                        }
                        None => {
                            trace!("interceptor_task -> close connection {:#?}", connection_id);
                            writers.remove(&connection_id);

                            let daemon_message = DaemonTcpOutgoing::Close(connection_id);
                            daemon_tx.send(daemon_message).await?
                        }
                    }
                }
                else => {
                    // We have no more data coming from any of the remote hosts.
                    warn!("interceptor_task -> no messages left");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Sends a `TcpOutgoingRequest` to the `interceptor_task`.
    pub(crate) async fn layer_message(
        &mut self,
        message: LayerTcpOutgoing,
    ) -> Result<(), AgentError> {
        trace!(
            "TcpOutgoingApi::layer_message -> layer_message {:#?}",
            message
        );
        Ok(self.layer_tx.send(message).await?)
    }

    /// Receives a `TcpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn daemon_message(&mut self) -> Result<DaemonTcpOutgoing, AgentError> {
        self.daemon_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}
