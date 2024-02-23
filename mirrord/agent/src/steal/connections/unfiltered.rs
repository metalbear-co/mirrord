use std::io::ErrorKind;

use mirrord_protocol::ConnectionId;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc::{Receiver, Sender},
};

use super::{ConnectionMessageIn, ConnectionMessageOut, ConnectionTaskError};
use crate::util::ClientId;

pub struct UnfilteredStealTask<T> {
    pub connection_id: ConnectionId,
    pub client_id: ClientId,
    pub stream: T,
    pub tx: Sender<ConnectionMessageOut>,
    pub rx: Receiver<ConnectionMessageIn>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> UnfilteredStealTask<T> {
    pub async fn run(mut self) -> Result<(), ConnectionTaskError> {
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

                        self.tx.send(message).await?;
                    }

                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}

                    Err(e) => break Err(e.into()),

                },

                message = self.rx.recv() => match message.ok_or(ConnectionTaskError::RecvError)? {
                    message if message.client_id() != self.client_id => {
                        tracing::error!(
                            client_id = message.client_id(),
                            expected_client_id = self.client_id,
                            connection_id = self.connection_id,
                            "Received message from an unexpected client",
                        );
                    }

                    ConnectionMessageIn::Raw { data, .. } => {
                        self.stream.write_all(&data).await?;
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
