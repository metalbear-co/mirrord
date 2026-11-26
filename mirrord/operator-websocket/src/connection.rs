use std::{
    fmt,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, SinkExt, Stream, StreamExt};
use hyper::{body::Bytes, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{ClientMessage, DecodeCtx};
use mirrord_protocol_io::{Client, ProtocolEndpoint};
use thiserror::Error;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message},
};

/// A mirrord-protocol connection over an HTTP-upgraded WebSocket.
///
/// `E` selects the incoming protocol message type. Each binary WebSocket message contains one
/// bincode-encoded protocol message.
///
/// [`OperatorConnection<Client>`] implements:
/// 1. [`Stream`] of [`mirrord_protocol::DaemonMessage`]s
/// 2. [`Sink`] of [`ClientMessage`]s
/// 3. [`Sink`] of [`Vec<u8>`]s ([`ClientMessage`]s pre-encoded with [`bincode`]) - mostly to fit
///    into the existing interfaces. Encoded messages are not verified in any way.
///
/// Other endpoints stream `E::InMsg` and accept pre-encoded [`Vec<u8>`] or [`Bytes`] payloads.
///
/// A close frame from the peer is an [`OperatorConnectionError::InvalidMessage`], unless its code
/// is one of [`Self::with_clean_close_codes`], which end the stream instead. Consumers such as
/// `mirrord_protocol_io`'s IO task treat both as the end of the connection, but log an error at
/// `ERROR`, so a peer that closes with a code meaning "done" must not look like a failure.
pub struct OperatorConnection<E = Client> {
    socket: WebSocketStream<TokioIo<Upgraded>>,
    clean_close_codes: &'static [u16],
    endpoint: PhantomData<fn() -> E>,
}

impl<E> OperatorConnection<E> {
    pub fn new(socket: WebSocketStream<TokioIo<Upgraded>>) -> Self {
        Self {
            socket,
            clean_close_codes: &[],
            endpoint: PhantomData,
        }
    }

    /// Ends the stream, instead of yielding an error, when the peer closes with one of `codes`.
    pub fn with_clean_close_codes(mut self, codes: &'static [u16]) -> Self {
        self.clean_close_codes = codes;
        self
    }
}

impl<E> fmt::Debug for OperatorConnection<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OperatorConnection")
            .field(&self.socket)
            .finish()
    }
}

impl<E: ProtocolEndpoint> Stream for OperatorConnection<E> {
    type Item = Result<E::InMsg, OperatorConnectionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            let msg = std::task::ready!(this.socket.poll_next_unpin(cx));
            let msg = match msg {
                Some(Ok(Message::Binary(msg))) => {
                    let msg = DecodeCtx::decode_from_bytes::<E::InMsg>(msg).map_err(From::from);
                    Some(msg)
                }
                Some(Ok(Message::Ping(..) | Message::Pong(..))) => {
                    // `tungstenite` can surface ping messages, but handles them automatically
                    continue;
                }
                Some(Ok(Message::Close(Some(frame))))
                    if this.clean_close_codes.contains(&u16::from(frame.code)) =>
                {
                    None
                }
                Some(Ok(msg @ (Message::Text(..) | Message::Frame(..) | Message::Close(..)))) => {
                    Some(Err(OperatorConnectionError::InvalidMessage(msg.into())))
                }
                Some(Err(error)) => Some(Err(OperatorConnectionError::WsError(error.into()))),
                None => None,
            };
            break Poll::Ready(msg);
        }
    }
}

impl Sink<ClientMessage> for OperatorConnection<Client> {
    type Error = OperatorConnectionError;

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_close_unpin(cx)
            .map_err(From::from)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_flush_unpin(cx)
            .map_err(From::from)
    }

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_ready_unpin(cx)
            .map_err(From::from)
    }

    fn start_send(self: Pin<&mut Self>, item: ClientMessage) -> Result<(), Self::Error> {
        let item = bincode::encode_to_vec(&item, bincode::config::standard())?;
        self.get_mut()
            .socket
            .start_send_unpin(Message::Binary(item.into()))
            .map_err(From::from)
    }
}

impl<E> Sink<Vec<u8>> for OperatorConnection<E> {
    type Error = OperatorConnectionError;

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_close_unpin(cx)
            .map_err(From::from)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_flush_unpin(cx)
            .map_err(From::from)
    }

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_ready_unpin(cx)
            .map_err(From::from)
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        self.get_mut()
            .socket
            .start_send_unpin(Message::Binary(item.into()))
            .map_err(From::from)
    }
}

impl<E> Sink<Bytes> for OperatorConnection<E> {
    type Error = OperatorConnectionError;

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_close_unpin(cx)
            .map_err(From::from)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_flush_unpin(cx)
            .map_err(From::from)
    }

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .socket
            .poll_ready_unpin(cx)
            .map_err(From::from)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        self.get_mut()
            .socket
            .start_send_unpin(Message::Binary(item))
            .map_err(From::from)
    }
}

/// Errors that can occur when working with [`OperatorConnection`].
#[derive(Error, Debug)]
pub enum OperatorConnectionError {
    #[error("bincode decode: {0}")]
    /// Failed to decode an incoming protocol message with [`bincode::de`].
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
