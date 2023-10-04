use std::io;

use mirrord_protocol::{
    outgoing::{tcp::LayerTcpOutgoing, LayerWrite},
    ClientMessage, ConnectionId,
};
use tokio::sync::mpsc::Receiver;

use super::PreparedSocket;
use crate::{agent_conn::AgentSender, protocol::NetProtocol};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InterceptorId {
    pub connection_id: ConnectionId,
    pub protocol: NetProtocol,
}

pub struct OutgoingInterceptor {
    agent_sender: AgentSender,
    connection_id: ConnectionId,
    protocol: NetProtocol,
    socket: PreparedSocket,
}

impl OutgoingInterceptor {
    pub fn new(
        agent_sender: AgentSender,
        connection_id: ConnectionId,
        protocol: NetProtocol,
        socket: PreparedSocket,
    ) -> Self {
        Self {
            agent_sender,
            connection_id,
            protocol,
            socket,
        }
    }
}

impl Component for OutgoingInterceptor {
    type Message = Vec<u8>;
    type Error = io::Error;
    type Id = InterceptorId;

    fn id(&self) -> Self::Id {
        InterceptorId {
            connection_id: self.connection_id,
            protocol: self.protocol,
        }
    }

    async fn run(self, mut message_rx: Receiver<Vec<u8>>) -> Result<(), Self::Error> {
        let mut connected_socket = self.socket.accept().await?;

        loop {
            tokio::select! {
                biased; // To allow local socket to be read before being closed

                read = connected_socket.receive() => {
                    match read {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => break Err(fail),
                        Ok(bytes) if bytes.len() == 0 => break Ok(()),
                        Ok(bytes) => {
                            let write = LayerWrite {
                                connection_id: self.connection_id,
                                bytes,
                            };
                            let outgoing_write = LayerTcpOutgoing::Write(write);

                            self.agent_sender.send(ClientMessage::TcpOutgoing(outgoing_write)).await;
                        }
                    }
                },

                bytes = message_rx.recv() => {
                    match bytes {
                        Some(bytes) => connected_socket.send(&bytes).await?,
                        None => break Ok(()),
                    }
                },
            }
        }
    }
}
