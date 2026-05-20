use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, SinkExt, Stream, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use thiserror::Error;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message},
};

/// [`mirrord_protocol`] connection established with the operator.
///
/// Implements:
/// 1. [`Stream`] of [`DaemonMessage`]s
/// 2. [`Sink`] of [`ClientMessage`]s
/// 3. [`Sink`] of [`Vec<u8>`]s ([`ClientMessage`]s pre-encoded with [`bincode`]) - mostly to fit
///    into the existing interfaces. Encoded messages are not verified in any way.
pub struct OperatorConnection(pub(super) WebSocketStream<TokioIo<Upgraded>>);

impl Stream for OperatorConnection {
    type Item = Result<DaemonMessage, OperatorConnectionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let item = match std::task::ready!(this.0.poll_next_unpin(cx)) {
            Some(Ok(Message::Binary(msg))) => {
                match bincode::decode_from_slice(&msg, bincode::config::standard()) {
                    Ok((message, _)) => Some(Ok(message)),
                    Err(error) => Some(Err(OperatorConnectionError::DecodeError(error))),
                }
            }
            // Operator only sends binary messages.
            Some(Ok(unexpected)) => Some(Err(OperatorConnectionError::InvalidMessage(
                unexpected.into(),
            ))),
            Some(Err(error)) => Some(Err(OperatorConnectionError::WsError(error.into()))),
            None => None,
        };

        Poll::Ready(item)
    }
}

impl Sink<ClientMessage> for OperatorConnection {
    type Error = OperatorConnectionError;

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().0.poll_close_unpin(cx).map_err(From::from)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().0.poll_flush_unpin(cx).map_err(From::from)
    }

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().0.poll_ready_unpin(cx).map_err(From::from)
    }

    fn start_send(self: Pin<&mut Self>, item: ClientMessage) -> Result<(), Self::Error> {
        let item = bincode::encode_to_vec(&item, bincode::config::standard())?;
        self.get_mut()
            .0
            .start_send_unpin(Message::Binary(item))
            .map_err(From::from)
    }
}

impl Sink<Vec<u8>> for OperatorConnection {
    type Error = OperatorConnectionError;

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().0.poll_close_unpin(cx).map_err(From::from)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().0.poll_flush_unpin(cx).map_err(From::from)
    }

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut().0.poll_ready_unpin(cx).map_err(From::from)
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        self.get_mut()
            .0
            .start_send_unpin(Message::Binary(item))
            .map_err(From::from)
    }
}

/// Errors that can occur when working with [`OperatorConnection`].
#[derive(Error, Debug)]
pub enum OperatorConnectionError {
    #[error("bincode decode: {0}")]
    /// Failed to decode a [`DaemonMessage`] with [`bincode::de`].
    DecodeError(#[from] bincode::error::DecodeError),
    /// Failed to encode a [`ClientMessage`] with [`bincode::enc`].
    #[error("bincode encode: {0}")]
    EncodeError(#[from] bincode::error::EncodeError),
    /// [`tungstenite`] WebSocket connection failed.
    #[error("tungstenite: {0}")]
    WsError(#[from] Box<tungstenite::Error>),
    /// Received an unexpected [`Message`] from the WebSocket connection.
    ///
    /// Only [`Message::Binary`] messages are expected.
    #[error("unexpected message: {0:?}")]
    InvalidMessage(Box<Message>),
}

impl From<tungstenite::Error> for OperatorConnectionError {
    fn from(error: tungstenite::Error) -> Self {
        Self::WsError(Box::new(error))
    }
}
