use std::ops::RangeInclusive;

use futures::{stream::FuturesUnordered, StreamExt};
use kameo::actor::ActorRef;
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ConnectionId, LogMessage, Port,
};
use tokio::sync::{
    mpsc::{self, Receiver, Sender},
    oneshot,
};
use tokio_stream::{
    wrappers::{errors::BroadcastStreamRecvError, BroadcastStream},
    StreamMap, StreamNotifyClose,
};

use super::messages::{SniffedConnection, SnifferCommand, SnifferCommandInner};
use crate::{
    error::AgentError,
    metrics::{
        MetricsActor, MetricsDecConnectionSubscription, MetricsDecPortSubscription,
        MetricsIncPortSubscription,
    },
    util::ClientId,
    watched_task::TaskStatus,
};

/// Interface used by clients to interact with the
/// [`TcpConnectionSniffer`](super::TcpConnectionSniffer). Multiple instances of this struct operate
/// on a single sniffer instance.
pub(crate) struct TcpSnifferApi {
    /// Id of the client using this struct.
    client_id: ClientId,
    /// Channel used to send commands to the [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    sender: Sender<SnifferCommand>,
    /// Channel used to receive messages from the
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    receiver: Receiver<SniffedConnection>,
    /// View on the sniffer task's status.
    task_status: TaskStatus,
    /// Currently sniffed connections.
    connections: StreamMap<ConnectionId, StreamNotifyClose<BroadcastStream<Vec<u8>>>>,
    /// Ids for sniffed connections.
    connection_ids_iter: RangeInclusive<ConnectionId>,
    /// [`LayerTcp::PortSubscribe`] requests in progress.
    subscriptions_in_progress: FuturesUnordered<oneshot::Receiver<Port>>,
    metrics: ActorRef<MetricsActor>,
}

impl TcpSnifferApi {
    /// Capacity for channel that will be used by
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer) to notify this struct about new
    /// connections.
    pub const CONNECTION_CHANNEL_SIZE: usize = 128;

    /// Create a new instance of this struct and connect it to a
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer) instance.
    /// * `client_id` - id of the client using this struct
    /// * `sniffer_sender` - channel used to send commands to the
    ///   [`TcpConnectionSniffer`](super::TcpConnectionSniffer)
    /// * `task_status` - handle to the [`TcpConnectionSniffer`](super::TcpConnectionSniffer) exit
    ///   status
    /// * `metrics` - used to send agent metrics messages to our metrics actor;
    pub async fn new(
        client_id: ClientId,
        sniffer_sender: Sender<SnifferCommand>,
        mut task_status: TaskStatus,
        metrics: ActorRef<MetricsActor>,
    ) -> Result<Self, AgentError> {
        let (sender, receiver) = mpsc::channel(Self::CONNECTION_CHANNEL_SIZE);

        let command = SnifferCommand {
            client_id,
            command: SnifferCommandInner::NewClient(sender),
        };
        if sniffer_sender.send(command).await.is_err() {
            return Err(task_status.unwrap_err().await);
        }

        Ok(Self {
            client_id,
            sender: sniffer_sender,
            receiver,
            task_status,
            connections: Default::default(),
            connection_ids_iter: (0..=ConnectionId::MAX),
            subscriptions_in_progress: Default::default(),
            metrics,
        })
    }

    /// Send the given command to the connected
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    async fn send_command(&mut self, command: SnifferCommandInner) -> Result<(), AgentError> {
        let command = SnifferCommand {
            client_id: self.client_id,
            command,
        };

        if self.sender.send(command).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Return the next message from the connected
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    pub async fn recv(&mut self) -> Result<(DaemonTcp, Option<LogMessage>), AgentError> {
        tokio::select! {
            conn = self.receiver.recv() => match conn {
                Some(conn) => {
                    let id = self.connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;

                    self.connections.insert(id, StreamNotifyClose::new(BroadcastStream::new(conn.data)));

                    Ok((
                        DaemonTcp::NewConnection(NewTcpConnection {
                            connection_id: id,
                            remote_address: conn.session_id.source_addr.into(),
                            local_address: conn.session_id.dest_addr.into(),
                            source_port: conn.session_id.source_port,
                            destination_port: conn.session_id.dest_port,
                        }),
                        None,
                    ))
                },

                None => {
                    Err(self.task_status.unwrap_err().await)
                },
            },

            Some((connection_id, bytes)) = self.connections.next() => match bytes {
                Some(Ok(bytes)) => {
                    Ok((
                        DaemonTcp::Data(TcpData {
                            connection_id,
                            bytes,
                        }),
                        None,
                    ))
                }

                Some(Err(BroadcastStreamRecvError::Lagged(missed_packets))) => {
                    let log = LogMessage::error(format!(
                        "failed to process on time {missed_packets} packet(s) from mirrored connection {connection_id}, closing connection"
                    ));

                    Ok((
                        DaemonTcp::Close(TcpClose { connection_id }),
                        Some(log),
                    ))
                }

                None => {
                    Ok((
                        DaemonTcp::Close(TcpClose { connection_id }),
                        None
                    ))
                }
            },

            Some(result) = self.subscriptions_in_progress.next() => match result {
                Ok(port) => Ok((DaemonTcp::SubscribeResult(Ok(port)), None)),
                Err(..) => {
                    Err(self.task_status.unwrap_err().await)
                }
            }
        }
    }

    /// Tansforms a [`LayerTcp`] message into a [`SnifferCommand`] and passes it to the connected
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    pub async fn handle_client_message(&mut self, message: LayerTcp) -> Result<(), AgentError> {
        match message {
            LayerTcp::PortSubscribe(port) => {
                let (tx, rx) = oneshot::channel();
                self.send_command(SnifferCommandInner::Subscribe(port, tx))
                    .await?;
                self.subscriptions_in_progress.push(rx);

                let _ = self
                    .metrics
                    .tell(MetricsIncPortSubscription)
                    .await
                    .inspect_err(|fail| tracing::trace!(?fail));

                Ok(())
            }

            LayerTcp::PortUnsubscribe(port) => {
                self.send_command(SnifferCommandInner::UnsubscribePort(port))
                    .await?;

                let _ = self
                    .metrics
                    .tell(MetricsDecPortSubscription)
                    .await
                    .inspect_err(|fail| tracing::trace!(?fail));

                Ok(())
            }

            LayerTcp::ConnectionUnsubscribe(connection_id) => {
                self.connections.remove(&connection_id);

                let _ = self
                    .metrics
                    .tell(MetricsDecConnectionSubscription)
                    .await
                    .inspect_err(|fail| tracing::trace!(?fail));

                Ok(())
            }
        }
    }
}
