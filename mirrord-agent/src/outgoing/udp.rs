use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    thread,
};

use mirrord_protocol::{
    outgoing::{udp::*, *},
    ConnectionId, ResponseError,
};
use streammap_ext::StreamMap;
use tokio::{
    io::AsyncWriteExt,
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        UdpSocket,
    },
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::{trace, warn};

use crate::{
    error::AgentError,
    runtime::set_namespace,
    util::{run_thread, IndexAllocator},
};

type Layer = LayerUdpOutgoing;
type Daemon = DaemonUdpOutgoing;

/// Handles (briefly) the `UdpOutgoingRequest` and `UdpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct UdpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<Result<(), AgentError>>,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<Layer>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Daemon>,
}

async fn connect(remote_address: SocketAddr) -> Result<UdpSocket, ResponseError> {
    let mirror_address = match remote_address {
        std::net::SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        std::net::SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };

    let mirror_socket = UdpSocket::bind(mirror_address).await?;
    mirror_socket.connect(remote_address).await?;

    Ok(mirror_socket)
}

impl UdpOutgoingApi {
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

            set_namespace(namespace).unwrap();
        }

        let mut allocator = IndexAllocator::default();

        // TODO: Right now we're manually keeping these 2 maps in sync (aviram suggested using
        // `Weak` for `writers`).
        let mut mirrors: HashMap<ConnectionId, UdpSocket> = HashMap::default();

        loop {
            select! {
                biased;

                // [layer] -> [agent]
                Some(layer_message) = layer_rx.recv() => {
                    trace!("interceptor_task -> layer_message {:?}", layer_message);
                    match layer_message {
                        // [user] -> [layer] -> [agent] -> [layer]
                        // `user` is asking us to connect to some remote host.
                        LayerUdpOutgoing::Connect(LayerConnect { remote_address }) => {
                            let daemon_connect = connect(remote_address)
                                    .await
                                    .map(|mirror_socket| {
                                        let connection_id = allocator
                                            .next_index()
                                            .ok_or_else(|| ResponseError::AllocationFailure("interceptor_task".to_string()))
                                            .unwrap() as ConnectionId;

                                        mirrors.insert(connection_id, mirror_socket);

                                        DaemonConnect {
                                            connection_id,
                                            remote_address,
                                        }
                                    });

                            let daemon_message = DaemonUdpOutgoing::Connect(daemon_connect);
                            daemon_tx.send(daemon_message).await?
                        }
                        // [user] -> [layer] -> [agent] -> [remote]
                        // `user` wrote some message to the remote host.
                        LayerUdpOutgoing::Write(LayerWrite {
                            connection_id,
                            bytes,
                        }) => {
                            let daemon_write = match mirrors
                                .get_mut(&connection_id)
                                .ok_or(ResponseError::NotFound(connection_id as usize))
                            {
                                Ok(mirror) => mirror
                                    .send(&bytes)
                                    .await
                                    .map_err(ResponseError::from),
                                Err(fail) => Err(fail),
                            };

                            if let Err(fail) = daemon_write {
                                warn!("LayerUdpOutgoing::Write -> Failed with {:#?}", fail);
                                mirrors.remove(&connection_id);

                                let daemon_message = DaemonUdpOutgoing::Close(connection_id);
                                daemon_tx.send(daemon_message).await?
                            }
                        }
                        // [layer] -> [agent]
                        // `layer` closed their interceptor stream.
                        LayerUdpOutgoing::Close(LayerClose { ref connection_id }) => {
                            mirrors.remove(connection_id);
                        }
                    }
                }

                // [remote] -> [agent] -> [layer] -> [user]
                // Read the data from one of the connected remote hosts, and forward the result back
                // to the `user`.
                // TODO(alex) [high] 2022-09-06: There is a `UdpFramed` in tokio-utils, that has
                // the `split` and behaves like `Stream`, so maybe use that?
                Some((connection_id, remote_read)) = readers.next() => {
                    trace!("interceptor_task -> read connection_id {:#?}", connection_id);

                    match remote_read {
                        Some(read) => {
                            let daemon_read = read
                                .map_err(ResponseError::from)
                                .map(|bytes| DaemonRead { connection_id, bytes: bytes.to_vec() });

                            let daemon_message = DaemonUdpOutgoing::Read(daemon_read);
                            daemon_tx.send(daemon_message).await?
                        }
                        None => {
                            trace!("interceptor_task -> close connection {:#?}", connection_id);
                            mirrors.remove(&connection_id);

                            let daemon_message = DaemonUdpOutgoing::Close(connection_id);
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

    /// Sends a `UdpOutgoingRequest` to the `interceptor_task`.
    pub(crate) async fn layer_message(
        &mut self,
        message: LayerUdpOutgoing,
    ) -> Result<(), AgentError> {
        trace!(
            "UdpOutgoingApi::layer_message -> layer_message {:#?}",
            message
        );
        Ok(self.layer_tx.send(message).await?)
    }

    /// Receives a `UdpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn daemon_message(&mut self) -> Result<DaemonUdpOutgoing, AgentError> {
        self.daemon_rx
            .recv()
            .await
            .ok_or(AgentError::ReceiverClosed)
    }
}
