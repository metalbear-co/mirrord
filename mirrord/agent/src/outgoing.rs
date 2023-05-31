use std::{collections::HashMap, thread, time::Duration};

use mirrord_protocol::{
    outgoing::{tcp::*, *},
    ConnectionId, RemoteError, ResponseError,
};
use socket_stream::SocketStream;
use streammap_ext::StreamMap;
use tokio::{
    io::{split, AsyncWriteExt, ReadHalf, WriteHalf},
    select,
    sync::mpsc::{self, Receiver, Sender},
    time::timeout,
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::{debug, warn};

use crate::{
    error::Result,
    util::{run_thread_in_namespace, IndexAllocator},
    watched_task::{TaskStatus, WatchedTask},
};

pub(crate) mod socket_stream;
pub(crate) mod udp;

type Layer = LayerTcpOutgoing;
type Daemon = DaemonTcpOutgoing;

/// Handles (briefly) the `TcpOutgoingRequest` and `TcpOutgoingResponse` messages, mostly the
/// passing of these messages to the `interceptor_task` thread.
pub(crate) struct TcpOutgoingApi {
    /// Holds the `interceptor_task`.
    _task: thread::JoinHandle<()>,

    /// Status of the `interceptor_task`.
    task_status: TaskStatus,

    /// Sends the `Layer` message to the `interceptor_task`.
    layer_tx: Sender<Layer>,

    /// Reads the `Daemon` message from the `interceptor_task`.
    daemon_rx: Receiver<Daemon>,
}

#[tracing::instrument(level = "trace", skip(allocator, writers, readers, daemon_tx))]
async fn layer_recv(
    layer_message: LayerTcpOutgoing,
    allocator: &mut IndexAllocator<ConnectionId, 100>,
    writers: &mut HashMap<ConnectionId, WriteHalf<SocketStream>>,
    readers: &mut StreamMap<ConnectionId, ReaderStream<ReadHalf<SocketStream>>>,
    daemon_tx: Sender<DaemonTcpOutgoing>,
    pid: Option<u64>,
) -> Result<()> {
    match layer_message {
        // [user] -> [layer] -> [agent] -> [remote host]
        // user is asking us to connect to some remote host.
        LayerTcpOutgoing::Connect(LayerConnect { remote_address }) => {
            // TODO(alex): `timeout` here works around the issue where golang tries to connect to an
            // invalid `IP:port` combination, and hangs until the go socket times out.
            let daemon_connect = timeout(
                Duration::from_millis(3000),
                SocketStream::connect(remote_address.clone(), pid),
            )
            .await
            .map_err(|elapsed| {
                warn!("interceptor_task -> Elapsed connect error {:#?}", elapsed);
                ResponseError::Remote(RemoteError::ConnectTimedOut(remote_address.clone()))
            })
            .map_err(From::from)
            .and_then(|remote_stream| {
                let remote_stream = remote_stream?;
                let agent_address = remote_stream.local_addr()?;
                let connection_id = allocator
                    .next_index()
                    .ok_or_else(|| ResponseError::AllocationFailure("layer_recv".to_string()))
                    .unwrap() as ConnectionId;

                // Split the `remote_stream` so we can keep reading
                // and writing from multiple hosts without blocking.
                let (read_half, write_half) = split(remote_stream);
                writers.insert(connection_id, write_half);
                readers.insert(connection_id, ReaderStream::new(read_half));

                Ok(DaemonConnect {
                    connection_id,
                    remote_address,
                    local_address: agent_address,
                })
            });

            let daemon_message = DaemonTcpOutgoing::Connect(daemon_connect);
            daemon_tx.send(daemon_message).await?
        }
        // [user] -> [layer] -> [agent] -> [remote]
        // `user` wrote some message to the remote host.
        LayerTcpOutgoing::Write(LayerWrite {
            connection_id,
            bytes,
        }) => {
            debug!(
                "connection_id {connection_id:?}, bytes_len {:?}",
                bytes.len()
            );
            let daemon_write = match writers
                .get_mut(&connection_id)
                .ok_or(ResponseError::NotFound(connection_id))
            {
                Ok(writer) => writer.write_all(&bytes).await.map_err(ResponseError::from),
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

    Ok(())
}

impl TcpOutgoingApi {
    const TASK_NAME: &'static str = "TcpOutgoing";

    pub(crate) fn new(pid: Option<u64>) -> Self {
        let (layer_tx, layer_rx) = mpsc::channel(1000);
        let (daemon_tx, daemon_rx) = mpsc::channel(1000);

        let watched_task = WatchedTask::new(
            Self::TASK_NAME,
            Self::interceptor_task(layer_rx, daemon_tx, pid),
        );
        let task_status = watched_task.status();
        let task = run_thread_in_namespace(
            watched_task.start(),
            Self::TASK_NAME.to_string(),
            pid,
            "net",
        );

        Self {
            _task: task,
            task_status,
            layer_tx,
            daemon_rx,
        }
    }

    /// Does the actual work for `Request`s and prepares the `Response`s:
    #[tracing::instrument(level = "trace", skip(layer_rx, daemon_tx))]
    async fn interceptor_task(
        mut layer_rx: Receiver<Layer>,
        daemon_tx: Sender<Daemon>,
        pid: Option<u64>,
    ) -> Result<()> {
        let mut allocator: IndexAllocator<ConnectionId, 100> = Default::default();

        // TODO: Right now we're manually keeping these 2 maps in sync (aviram suggested using
        // `Weak` for `writers`).
        let mut writers: HashMap<ConnectionId, WriteHalf<SocketStream>> = HashMap::default();
        let mut readers: StreamMap<ConnectionId, ReaderStream<ReadHalf<SocketStream>>> =
            StreamMap::default();

        loop {
            select! {
                biased;

                // [layer] -> [agent]
                Some(layer_message) = layer_rx.recv() => {
                    layer_recv(layer_message, &mut allocator, &mut writers, &mut readers, daemon_tx.clone(), pid).await?
                }

                // [remote] -> [agent] -> [layer] -> [user]
                // Read the data from one of the connected remote hosts, and forward the result back
                // to the `user`.
                Some((connection_id, remote_read)) = readers.next() => {
                    debug!("interceptor_task -> read connection_id {:#?}", connection_id);

                    match remote_read {
                        Some(read) => {
                            let daemon_read = read
                                .map_err(ResponseError::from)
                                .map(|bytes| DaemonRead { connection_id, bytes: bytes.to_vec() });

                            let daemon_message = DaemonTcpOutgoing::Read(daemon_read);
                            daemon_tx.send(daemon_message).await?
                        }
                        None => {
                            debug!("interceptor_task -> close connection {:#?}", connection_id);
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
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn layer_message(&mut self, message: LayerTcpOutgoing) -> Result<()> {
        if self.layer_tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Receives a `TcpOutgoingResponse` from the `interceptor_task`.
    pub(crate) async fn daemon_message(&mut self) -> Result<DaemonTcpOutgoing> {
        match self.daemon_rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.unwrap_err().await),
        }
    }
}
