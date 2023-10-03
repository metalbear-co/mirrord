use std::io;

use mirrord_protocol::{
    outgoing::{tcp::LayerTcpOutgoing, LayerWrite},
    ClientMessage, ConnectionId,
};
use tokio::sync::mpsc::Receiver;

use super::PreparedSocket;
use crate::{agent_conn::AgentSender, protocol::NetProtocol, system::Component};

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

// async fn close_remote_stream(&self) -> Result<()> {
//     let message = self.protocol.wrap_agent_close(self.connection_id);

//     self.agent_sender.send(message).await.map_err(Into::into)
// }

// pub async fn run(mut self, prepared_socket: PreparedSocket) -> Result<()> {
//     let mut connected_socket = prepared_socket.accept().await?;

//     loop {
//         tokio::select! {
//             biased; // To allow local socket to be read before being closed

//             read = connected_socket.receive() => {
//                 match read {
//                     Err(IntProxyError::Io(fail)) if fail.kind() == std::io::ErrorKind::WouldBlock
// => {                         continue;
//                     },
//                     Err(fail) => {
//                         tracing::info!("failed reading mirror_stream with {fail:#?}");
//                         self.close_remote_stream().await?;
//                         break Err(fail);
//                     }
//                     Ok(bytes) if bytes.len() == 0 => {
//                         tracing::trace!("interceptor_task -> stream {} has no more data,
// closing!", self.connection_id);                         self.close_remote_stream().await?;
//                         break Ok(());
//                     }
//                     Ok(bytes) => {
//                         let write = LayerWrite {
//                             connection_id: self.connection_id,
//                             bytes,
//                         };
//                         let outgoing_write = LayerTcpOutgoing::Write(write);

//
// self.agent_sender.send(ClientMessage::TcpOutgoing(outgoing_write)).await?;                     }
//                 }
//             },

//             bytes = self.task_rx.recv() => {
//                 match bytes {
//                     Some(bytes) => {
//                         // Writes the data sent by `agent` (that came from the actual remote
//                         // stream) to our interceptor socket. When the user tries to read the
//                         // remote data, this'll be what they receive.
//                         connected_socket.send(&bytes).await?;
//                     },
//                     None => {
//                         tracing::warn!("interceptor_task -> exiting due to remote stream
// closed!");                         break Ok(());
//                     }
//                 }
//             },
//         }
//     }
// }
