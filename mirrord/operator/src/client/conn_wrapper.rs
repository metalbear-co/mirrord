use futures::{Sink, SinkExt, Stream, StreamExt};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_tungstenite::tungstenite::{self, Message};

#[derive(Error, Debug)]
enum ConnectionWrapperError {
    #[error(transparent)]
    DecodeError(#[from] bincode::error::DecodeError),
    #[error(transparent)]
    EncodeError(#[from] bincode::error::EncodeError),
    #[error(transparent)]
    WsError(#[from] tungstenite::Error),
    #[error("invalid message: {0:?}")]
    InvalidMessage(Message),
    #[error("message channel is closed")]
    ChannelClosed,
}

pub struct ConnectionWrapper<T> {
    connection: T,
    client_rx: Receiver<ClientMessage>,
    daemon_tx: Sender<DaemonMessage>,
    protocol_version: Option<semver::Version>,
}

impl<T> ConnectionWrapper<T>
where
    for<'stream> T: Stream<Item = Result<Message, tungstenite::Error>>
        + Sink<Message, Error = tungstenite::Error>
        + Send
        + Unpin
        + 'stream,
{
    const CONNECTION_CHANNEL_SIZE: usize = 1000;

    pub fn wrap(
        connection: T,
        protocol_version: Option<semver::Version>,
    ) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
        let (client_tx, client_rx) = mpsc::channel(Self::CONNECTION_CHANNEL_SIZE);
        let (daemon_tx, daemon_rx) = mpsc::channel(Self::CONNECTION_CHANNEL_SIZE);

        let connection_wrapper = ConnectionWrapper {
            protocol_version,
            connection,
            client_rx,
            daemon_tx,
        };

        tokio::spawn(async move {
            match connection_wrapper.start().await {
                Ok(()) | Err(ConnectionWrapperError::ChannelClosed) => {}
                Err(error) => tracing::error!(%error, "Operator connection failed"),
            }
        });

        (client_tx, daemon_rx)
    }

    async fn handle_client_message(
        &mut self,
        client_message: ClientMessage,
    ) -> Result<(), ConnectionWrapperError> {
        let payload = bincode::encode_to_vec(client_message, bincode::config::standard())?;

        self.connection.send(payload.into()).await?;

        Ok(())
    }

    async fn handle_daemon_message(
        &mut self,
        daemon_message: Result<Message, tungstenite::Error>,
    ) -> Result<(), ConnectionWrapperError> {
        match daemon_message? {
            Message::Binary(payload) => {
                let (daemon_message, _) = bincode::decode_from_slice::<DaemonMessage, _>(
                    &payload,
                    bincode::config::standard(),
                )?;

                self.daemon_tx
                    .send(daemon_message)
                    .await
                    .map_err(|_| ConnectionWrapperError::ChannelClosed)
            }
            message => Err(ConnectionWrapperError::InvalidMessage(message)),
        }
    }

    async fn start(mut self) -> Result<(), ConnectionWrapperError> {
        loop {
            tokio::select! {
                client_message = self.client_rx.recv() => {
                    match client_message {
                        Some(ClientMessage::SwitchProtocolVersion(version)) => {
                            if let Some(operator_protocol_version) = self.protocol_version.as_ref() {
                                self.handle_client_message(ClientMessage::SwitchProtocolVersion(operator_protocol_version.min(&version).clone())).await?;
                            } else {
                                self.daemon_tx
                                    .send(DaemonMessage::SwitchProtocolVersionResponse(
                                        "1.2.1".parse().expect("Bad static version"),
                                    ))
                                    .await
                                    .map_err(|_| ConnectionWrapperError::ChannelClosed)?;
                            }
                        }
                        Some(client_message) => self.handle_client_message(client_message).await?,
                        None => break,
                    }
                }

                daemon_message = self.connection.next() => match daemon_message {
                    Some(daemon_message) => self.handle_daemon_message(daemon_message).await?,
                    None => break,
                },
            }
        }

        let _ = self.connection.send(Message::Close(None)).await;

        Ok(())
    }
}
