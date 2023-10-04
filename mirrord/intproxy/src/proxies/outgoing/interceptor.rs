use std::io;

use mirrord_protocol::ConnectionId;

use super::PreparedSocket;
use crate::{
    protocol::NetProtocol,
    task_manager::{MessageBus, Task},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InterceptorId {
    pub connection_id: ConnectionId,
    pub protocol: NetProtocol,
}

pub struct OutgoingInterceptor {
    connection_id: ConnectionId,
    protocol: NetProtocol,
    socket: PreparedSocket,
}

impl OutgoingInterceptor {
    pub fn new(connection_id: ConnectionId, protocol: NetProtocol, socket: PreparedSocket) -> Self {
        Self {
            connection_id,
            protocol,
            socket,
        }
    }
}

pub enum OutgoingInterceptorMessage {
    Bytes(Vec<u8>),
    AgentClosed,
}

impl Task for OutgoingInterceptor {
    type MessageOut = Vec<u8>;
    type Error = io::Error;
    type Id = InterceptorId;
    type MessageIn = OutgoingInterceptorMessage;

    fn id(&self) -> Self::Id {
        InterceptorId {
            connection_id: self.connection_id,
            protocol: self.protocol,
        }
    }

    async fn run(self, messages: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut connected_socket = self.socket.accept().await?;

        loop {
            tokio::select! {
                biased; // To allow local socket to be read before being closed

                read = connected_socket.receive() => match read {
                    Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    },
                    Err(fail) => break Err(fail),
                    Ok(bytes) if bytes.len() == 0 => break Ok(()),
                    Ok(bytes) => messages.send(bytes).await,
                },

                message = messages.recv() => match message {
                    OutgoingInterceptorMessage::Bytes(b) => connected_socket.send(&b).await?,
                    OutgoingInterceptorMessage::AgentClosed => break Ok(()),
                },
            }
        }
    }
}
