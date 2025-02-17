use std::io::ErrorKind;

use bytes::BytesMut;
use mirrord_protocol::ConnectionId;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc::{Receiver, Sender},
};

use super::{
    ConnectionMessageIn, ConnectionMessageOut, ConnectionTaskError,
    STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION,
};
use crate::util::ClientId;

/// Manages an unfiltered stolen connection.
pub struct UnfilteredStealTask<T> {
    pub connection_id: ConnectionId,
    /// Id of the stealer client that exclusively owns this connection.
    pub client_id: ClientId,
    /// Stolen connection as a raw IO stream.
    pub stream: T,
}

impl<T> Drop for UnfilteredStealTask<T> {
    fn drop(&mut self) {
        STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> UnfilteredStealTask<T> {
    pub(crate) fn new(connection_id: ConnectionId, client_id: ClientId, stream: T) -> Self {
        STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Self {
            connection_id,
            client_id,
            stream,
        }
    }

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
        let mut buf = BytesMut::with_capacity(64 * 1024);
        let mut reading_closed = false;

        loop {
            tokio::select! {
                read = self.stream.read_buf(&mut buf), if !reading_closed => match read {
                    Ok(..) => {
                        if buf.is_empty() {
                            STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                            tracing::trace!(
                                client_id = self.client_id,
                                connection_id = self.connection_id,
                                "Connection shutdown, sending 0-sized read to the client",
                            );

                            reading_closed = true;
                        }

                        let message = ConnectionMessageOut::Raw {
                            client_id: self.client_id,
                            connection_id: self.connection_id,
                            data: buf.to_vec(),
                        };

                        tx.send(message).await?;

                        buf.clear();
                    }

                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}

                    Err(e) => {
                        STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

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

                    ConnectionMessageIn::Raw { data, .. } => {
                        let res = if data.is_empty() {
                            STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                            tracing::trace!(
                                client_id = self.client_id,
                                connection_id = self.connection_id,
                                "Received a 0-sized write from the client, shutting down connection",
                            );

                            self.stream.shutdown().await
                        } else {
                            self.stream.write_all(&data).await
                        };

                        if let Err(e) = res {
                            STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                            tx.send(ConnectionMessageOut::Closed {
                                client_id: self.client_id,
                                connection_id: self.connection_id
                            }).await?;

                            break Err(e.into());
                        }
                    },

                    ConnectionMessageIn::Response { request_id, .. } => {
                        tracing::trace!(
                            connection_id = self.connection_id,
                            request_id,
                            "Received an unexpected HTTP message",
                        );
                    },

                    ConnectionMessageIn::Unsubscribed { .. } => {
                        STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    use super::*;

    #[tokio::test]
    async fn two_way_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (mut client_stream, (server_stream, _)) =
            tokio::try_join!(TcpStream::connect(addr), listener.accept(),).unwrap();

        let (in_tx, mut in_rx) = mpsc::channel(8);
        let (out_tx, mut out_rx) = mpsc::channel(8);

        let handle = tokio::spawn(async move {
            let task = UnfilteredStealTask {
                connection_id: 1,
                client_id: 2,
                stream: server_stream,
            };

            task.run(out_tx, &mut in_rx).await.unwrap();
        });

        tokio::try_join!(client_stream.write_all(b"bytes from peer"), async {
            let _ = in_tx
                .send(ConnectionMessageIn::Raw {
                    data: b"bytes from client".to_vec(),
                    client_id: 2,
                })
                .await;
            Ok(())
        })
        .unwrap();

        let msg = out_rx.recv().await.unwrap();
        let data = match msg {
            ConnectionMessageOut::Raw {
                client_id: 2,
                connection_id: 1,
                data,
            } => data,
            other => unreachable!("unexpected message: {other:?}"),
        };
        assert_eq!(data, b"bytes from peer");

        let mut client_read = *b"bytes from client";
        let bytes_read = client_stream.read_exact(&mut client_read).await.unwrap();
        assert_eq!(bytes_read, client_read.len());
        assert_eq!(&client_read, b"bytes from client");

        in_tx
            .send(ConnectionMessageIn::Unsubscribed { client_id: 2 })
            .await
            .unwrap();

        handle.await.unwrap();

        let msg = out_rx.recv().await;
        assert!(msg.is_none());
    }
}
