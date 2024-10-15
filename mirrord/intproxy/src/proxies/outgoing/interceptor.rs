//! [`BackgroundTask`] used by [`OutgoingProxy`](super::OutgoingProxy) to manage a single
//! intercepted connection.

use std::io;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    proxies::outgoing::net_protocol_ext::PreparedSocket,
};

/// Manages a single intercepted connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`OutgoingProxy`](super::OutgoingProxy)
/// to manage individual connections.
pub struct Interceptor {
    socket: PreparedSocket,
}

impl Interceptor {
    /// Creates a new instance. This instance will use the provided [`PreparedSocket`] to accept the
    /// layer's connection and manage it.
    pub fn new(socket: PreparedSocket) -> Self {
        Self { socket }
    }
}

impl BackgroundTask for Interceptor {
    type Error = io::Error;
    type MessageIn = Vec<u8>;
    type MessageOut = Vec<u8>;

    /// Accepts one connection the owned [`PreparedSocket`] and transparently proxies bytes between
    /// the [`MessageBus`] and the new
    /// [`ConnectedSocket`](crate::proxies::outgoing::net_protocol_ext::ConnectedSocket).
    ///
    /// # Notes
    ///
    /// 1. When the peer shuts down writing, a single 0-sized read is sent through the
    ///    [`MessageBus`]. This is to notify the agent about the shutdown condition.
    ///
    /// 2. A 0-sized read received from the [`MessageBus`] is treated as a shutdown on the agent
    ///    side. Connection with the peer is shut down as well.
    ///
    /// 3. This implementation exits only when an error is encountered or the [`MessageBus`] is
    ///    closed.
    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut connected_socket = self.socket.accept().await?;
        let mut reading_closed = false;

        loop {
            tokio::select! {
                read = connected_socket.receive(), if !reading_closed => match read {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    },
                    Err(e) => break Err(e),
                    Ok(bytes) => {
                        if bytes.is_empty() {
                            tracing::trace!("outgoing interceptor -> layer shutdown, sending a 0-sized read to inform the agent");
                            reading_closed = true;
                        }
                        message_bus.send(bytes).await
                    },
                },

                msg = message_bus.recv() => match msg {
                    Some(bytes) => {
                        if bytes.is_empty() {
                            tracing::trace!("outgoing interceptor -> agent shutdown, shutting down connection with layer");
                            connected_socket.shutdown().await?;
                        } else {
                            connected_socket.send(&bytes).await?;
                        }
                    }

                    None => {
                        tracing::trace!("outgoing interceptor -> no more messages from the agent, exiting");
                        break Ok(())
                    }
                },
            }
        }
    }
}
