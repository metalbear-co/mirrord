use mirrord_protocol::{
    outgoing::{tcp::LayerTcpOutgoing, LayerWrite},
    ClientMessage, ConnectionId,
};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::PreparedSocket;
use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    protocol::NetProtocol,
};

pub struct InterceptorTaskHandle(Sender<Vec<u8>>);

impl InterceptorTaskHandle {
    pub async fn send(&self, data: Vec<u8>) -> Result<()> {
        self.0
            .send(data)
            .await
            .map_err(|_| IntProxyError::OutgoingInterceptorFailed)
    }
}

pub struct InterceptorTask {
    agent_sender: AgentSender,
    connection_id: ConnectionId,
    protocol: NetProtocol,
    task_rx: Receiver<Vec<u8>>,
    task_tx: Option<Sender<Vec<u8>>>,
}

impl InterceptorTask {
    pub fn new(
        agent_sender: AgentSender,
        connection_id: ConnectionId,
        protocol: NetProtocol,
        channel_size: usize,
    ) -> Self {
        let (task_tx, task_rx) = mpsc::channel(channel_size);

        Self {
            agent_sender,
            connection_id,
            protocol,
            task_rx,
            task_tx: task_tx.into(),
        }
    }

    pub fn handle(&self) -> InterceptorTaskHandle {
        let tx = self
            .task_tx
            .as_ref()
            .expect("interceptor sender should not be dropped before the task is run")
            .clone();

        InterceptorTaskHandle(tx)
    }

    async fn close_remote_stream(&self) -> Result<()> {
        let message = self.protocol.wrap_agent_close(self.connection_id);

        self.agent_sender.send(message).await.map_err(Into::into)
    }

    pub async fn run(mut self, prepared_socket: PreparedSocket) -> Result<()> {
        let mut connected_socket = prepared_socket.accept().await?;

        loop {
            tokio::select! {
                biased; // To allow local socket to be read before being closed

                read = connected_socket.receive() => {
                    match read {
                        Err(IntProxyError::Io(fail)) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => {
                            tracing::info!("failed reading mirror_stream with {fail:#?}");
                            self.close_remote_stream().await?;
                            break Err(fail);
                        }
                        Ok(bytes) if bytes.len() == 0 => {
                            tracing::trace!("interceptor_task -> stream {} has no more data, closing!", self.connection_id);
                            self.close_remote_stream().await?;
                            break Ok(());
                        }
                        Ok(bytes) => {
                            let write = LayerWrite {
                                connection_id: self.connection_id,
                                bytes,
                            };
                            let outgoing_write = LayerTcpOutgoing::Write(write);

                            self.agent_sender.send(ClientMessage::TcpOutgoing(outgoing_write)).await?;
                        }
                    }
                },

                bytes = self.task_rx.recv() => {
                    match bytes {
                        Some(bytes) => {
                            // Writes the data sent by `agent` (that came from the actual remote
                            // stream) to our interceptor socket. When the user tries to read the
                            // remote data, this'll be what they receive.
                            connected_socket.send(&bytes).await?;
                        },
                        None => {
                            tracing::warn!("interceptor_task -> exiting due to remote stream closed!");
                            break Ok(());
                        }
                    }
                },
            }
        }
    }
}
