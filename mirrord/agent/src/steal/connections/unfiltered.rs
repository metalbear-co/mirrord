use std::io::ErrorKind;

use mirrord_protocol::ConnectionId;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc::{Receiver, Sender},
};

use super::{ConnectionMessageIn, ConnectionMessageOut, ConnectionTaskError};
use crate::util::ClientId;

/// Manages an unfiltered stolen connection.
pub struct UnfilteredStealTask<T> {
    pub connection_id: ConnectionId,
    /// Id of the stealer client that exclusively owns this connection.
    pub client_id: ClientId,
    /// Stolen connection as a raw IO stream.
    pub stream: T,
}

impl<T: AsyncRead + AsyncWrite + Unpin> UnfilteredStealTask<T> {
    /// Runs this task until the managed connection is closed.
    ///
    /// # Note
    ///
    /// Does not send [`ConnectionMessageOut::SubscribedTcp`], assuming that this message has
    /// already been sent to the client. This allows this code to be used for both unfiltered
    /// and upgraded connections.
    pub async fn run(
        mut self,
        tx: Sender<ConnectionMessageOut>,
        rx: &mut Receiver<ConnectionMessageIn>,
    ) -> Result<(), ConnectionTaskError> {
        let mut buffer = [0_u8; 1024];
        let mut reading_closed = false;

        loop {
            tokio::select! {
                read = self.stream.read(&mut buffer), if !reading_closed => match read {
                    Ok(0) => {
                        reading_closed = true;
                    }

                    Ok(bytes_read) => {
                        let data = buffer
                            .get(..bytes_read)
                            .expect("count of bytes read is out of range")
                            .to_vec();

                        let message = ConnectionMessageOut::Raw {
                            client_id: self.client_id,
                            connection_id: self.connection_id,
                            data,
                        };

                        tx.send(message).await?;
                    }

                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}

                    Err(e) => {
                        tx.send(ConnectionMessageOut::Closed {
                            client_id: self.client_id,
                            connection_id: self.connection_id
                        }).await?;

                        break Err(e.into());
                    }

                },

                message = rx.recv() => match message.ok_or(ConnectionTaskError::RecvError)? {
                    message if message.client_id() != self.client_id => {
                        tracing::error!(
                            client_id = message.client_id(),
                            expected_client_id = self.client_id,
                            connection_id = self.connection_id,
                            "Received message from an unexpected client",
                        );
                    }

                    ConnectionMessageIn::Raw { data, .. } => match self.stream.write_all(&data).await {
                        Ok(..) => {},
                        Err(e) => {
                            tx.send(ConnectionMessageOut::Closed {
                                client_id: self.client_id,
                                connection_id: self.connection_id
                            }).await?;

                            break Err(e.into());
                        }
                    },

                    ConnectionMessageIn::Response { request_id, .. } | ConnectionMessageIn::ResponseFailed { request_id, .. } => {
                        tracing::trace!(
                            connection_id = self.connection_id,
                            request_id,
                            "Received an unexpected HTTP message",
                        );
                    },

                    ConnectionMessageIn::Unsubscribed { .. } => {
                        return Ok(());
                    }
                }
            }
        }
    }
}
