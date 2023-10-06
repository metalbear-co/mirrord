use std::{fmt, io};

use mirrord_protocol::ConnectionId;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    protocol::NetProtocol,
    proxies::outgoing::protocols::PreparedSocket,
};

pub struct Interceptor {
    socket: PreparedSocket,
}

impl Interceptor {
    pub fn new(socket: PreparedSocket) -> Self {
        Self { socket }
    }
}

impl BackgroundTask for Interceptor {
    type Error = io::Error;
    type MessageIn = Vec<u8>;
    type MessageOut = Vec<u8>;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut connected_socket = self.socket.accept().await?;

        loop {
            tokio::select! {
                biased; // To allow local socket to be read before being closed

                read = connected_socket.receive() => match read {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    },
                    Err(e) => break Err(e),
                    Ok(bytes) if bytes.len() == 0 => break Ok(()),
                    Ok(bytes) => message_bus.send(bytes).await,
                },

                bytes = message_bus.recv() => match bytes {
                    Some(bytes) => connected_socket.send(&bytes).await?,
                    None => break Ok(()),
                },
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InterceptorId {
    pub connection_id: ConnectionId,
    pub protocol: NetProtocol,
}

impl fmt::Display for InterceptorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "outgoing interceptor {}-{}",
            self.connection_id, self.protocol
        )
    }
}
