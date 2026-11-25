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
    tungstenite::{self, Message, protocol::CloseFrame},
};

/// The close code and reason with which the sessions-manager's data plane closes one side of a
/// room after the other side left it.
///
/// That is how a sessions-manager connection ends normally, so [`OperatorConnection`] reports it
/// as the end of its stream, as for a connection that ended without a close frame, and not as an
/// [`OperatorConnectionError`]. Other close frames, such as the policy close of a refused claim,
/// stay errors that name why the connection closed.
const PEER_LEFT_CLOSE_CODE: u16 = 4002;
const PEER_LEFT_CLOSE_REASON: &str = "peer-left";

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
pub struct OperatorConnection<E = Client>(
    WebSocketStream<TokioIo<Upgraded>>,
    PhantomData<fn() -> E>,
);

impl<E> OperatorConnection<E> {
    pub fn new(socket: WebSocketStream<TokioIo<Upgraded>>) -> Self {
        Self(socket, PhantomData)
    }
}

impl<E> fmt::Debug for OperatorConnection<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OperatorConnection")
            .field(&self.0)
            .finish()
    }
}

impl<E: ProtocolEndpoint> Stream for OperatorConnection<E> {
    type Item = Result<E::InMsg, OperatorConnectionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            let msg = std::task::ready!(this.0.poll_next_unpin(cx));
            let msg = match msg {
                Some(Ok(Message::Binary(msg))) => {
                    let msg = DecodeCtx::decode_from_bytes::<E::InMsg>(msg).map_err(From::from);
                    Some(msg)
                }
                Some(Ok(Message::Ping(..) | Message::Pong(..))) => {
                    // `tungstenite` can surface ping messages, but handles them automatically
                    continue;
                }
                Some(Ok(Message::Close(Some(frame)))) if is_peer_left(&frame) => None,
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
            .start_send_unpin(Message::Binary(item.into()))
            .map_err(From::from)
    }
}

impl<E> Sink<Vec<u8>> for OperatorConnection<E> {
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
            .start_send_unpin(Message::Binary(item.into()))
            .map_err(From::from)
    }
}

impl<E> Sink<Bytes> for OperatorConnection<E> {
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

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        self.get_mut()
            .0
            .start_send_unpin(Message::Binary(item))
            .map_err(From::from)
    }
}

/// Whether `frame` is the sessions-manager's close after the other side of the room left (see
/// [`PEER_LEFT_CLOSE_CODE`]).
fn is_peer_left(frame: &CloseFrame) -> bool {
    u16::from(frame.code) == PEER_LEFT_CLOSE_CODE && frame.reason == PEER_LEFT_CLOSE_REASON
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
