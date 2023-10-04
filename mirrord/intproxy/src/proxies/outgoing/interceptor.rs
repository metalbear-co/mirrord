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

pub enum OutgoingInterceptorIn {
    Bytes(Vec<u8>),
    AgentClosed,
}

pub enum OutgoingInterceptorOut {
    Bytes(Vec<u8>),
    LayerClosed,
}

impl Task for OutgoingInterceptor {
    type Error = io::Error;
    type Id = InterceptorId;
    type MessageIn = OutgoingInterceptorIn;
    type MessageOut = OutgoingInterceptorOut;

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
                    Ok(bytes) if bytes.len() == 0 => {
                        messages.send(OutgoingInterceptorOut::LayerClosed).await;
                        break Ok(());
                    },
                    Ok(bytes) => messages.send(OutgoingInterceptorOut::Bytes(bytes)).await,
                },

                message = messages.recv() => {
                    let Some(message) = message else {
                        break Ok(());
                    };

                    match message {
                        OutgoingInterceptorIn::Bytes(b) => connected_socket.send(&b).await?,
                        OutgoingInterceptorIn::AgentClosed => break Ok(()),
                    }
                },
            }
        }
    }
}
