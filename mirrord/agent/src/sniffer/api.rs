use std::ops::RangeInclusive;

use futures::{stream::FuturesUnordered, StreamExt};
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp, NewTcpConnectionV1, TcpClose, TcpData},
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

use super::{
    messages::{SniffedConnection, SnifferCommand, SnifferCommandInner},
    AgentResult,
};
use crate::{
    error::AgentError,
    util::{remote_runtime::BgTaskStatus, ClientId},
};

/// Interface used by clients to interact with the
/// [`TcpConnectionSniffer`](super::TcpConnectionSniffer). Multiple instances of this struct operate
/// on a single sniffer instance.
///
/// Enabled by the `mirror` feature for incoming traffic.
pub(crate) struct TcpSnifferApi {
    /// Id of the client using this struct.
    client_id: ClientId,
    /// Channel used to send commands to the [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    sender: Sender<SnifferCommand>,
    /// Channel used to receive messages from the
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    receiver: Receiver<SniffedConnection>,
    /// View on the sniffer task's status.
    task_status: BgTaskStatus,
    /// Currently sniffed connections.
    connections: StreamMap<ConnectionId, StreamNotifyClose<BroadcastStream<Vec<u8>>>>,
    /// Ids for sniffed connections.
    connection_ids_iter: RangeInclusive<ConnectionId>,
    /// [`LayerTcp::PortSubscribe`] requests in progress.
    subscriptions_in_progress: FuturesUnordered<oneshot::Receiver<Port>>,
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
    pub async fn new(
        client_id: ClientId,
        sniffer_sender: Sender<SnifferCommand>,
        task_status: BgTaskStatus,
    ) -> AgentResult<Self> {
        let (sender, receiver) = mpsc::channel(Self::CONNECTION_CHANNEL_SIZE);

        let command = SnifferCommand {
            client_id,
            command: SnifferCommandInner::NewClient(sender),
        };
        if sniffer_sender.send(command).await.is_err() {
            return Err(task_status.wait_assert_running().await);
        }

        Ok(Self {
            client_id,
            sender: sniffer_sender,
            receiver,
            task_status,
            connections: Default::default(),
            connection_ids_iter: (0..=ConnectionId::MAX),
            subscriptions_in_progress: Default::default(),
        })
    }

    /// Send the given command to the connected
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    async fn send_command(&mut self, command: SnifferCommandInner) -> AgentResult<()> {
        let command = SnifferCommand {
            client_id: self.client_id,
            command,
        };

        if self.sender.send(command).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }

    /// Return the next message from the connected
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    pub async fn recv(&mut self) -> AgentResult<(DaemonTcp, Option<LogMessage>)> {
        tokio::select! {
            conn = self.receiver.recv() => match conn {
                Some(conn) => {
                    let id = self.connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;

                    self.connections.insert(id, StreamNotifyClose::new(BroadcastStream::new(conn.data)));

                    Ok((
                        DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
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
                    Err(self.task_status.wait_assert_running().await)
                },
            },

            Some((connection_id, bytes)) = self.connections.next() => match bytes {
                Some(Ok(bytes)) => {
                    Ok((
                        DaemonTcp::Data(TcpData {
                            connection_id,
                            bytes: bytes.into(),
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
                    Err(self.task_status.wait_assert_running().await)
                }
            }
        }
    }

    /// Tansforms a [`LayerTcp`] message into a [`SnifferCommand`] and passes it to the connected
    /// [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
    pub async fn handle_client_message(&mut self, message: LayerTcp) -> AgentResult<()> {
        match message {
            LayerTcp::PortSubscribe(port) => {
                let (tx, rx) = oneshot::channel();
                self.send_command(SnifferCommandInner::Subscribe(port, tx))
                    .await?;
                self.subscriptions_in_progress.push(rx);
                Ok(())
            }

            LayerTcp::PortUnsubscribe(port) => {
                self.send_command(SnifferCommandInner::UnsubscribePort(port))
                    .await?;
                Ok(())
            }

            LayerTcp::ConnectionUnsubscribe(connection_id) => {
                self.connections.remove(&connection_id);
                Ok(())
            }
        }
    }
}
