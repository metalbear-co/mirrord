use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::{Decoder, Encoder, Framed};
use bytes::BytesMut;
use futures::{Sink, SinkExt, Stream, StreamExt};
use hyper::{body::Bytes, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use mirrord_quic::session::SessionStream;
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
///
/// The variants carry the same message exchange over different transports. Which one a session
/// gets is decided when it connects, and nothing downstream is meant to care.
pub enum OperatorConnection {
    /// Proxied by the Kubernetes API server as a websocket. Always available.
    WebSocket(Box<WebSocketStream<TokioIo<Upgraded>>>),
    /// Dialed directly over QUIC. Available when the installation exposes an endpoint for it.
    Direct(Box<Framed<SessionStream, SessionCodec>>),
}

impl OperatorConnection {
    pub(super) fn websocket(stream: WebSocketStream<TokioIo<Upgraded>>) -> Self {
        Self::WebSocket(Box::new(stream))
    }

    pub(super) fn direct(stream: SessionStream) -> Self {
        Self::Direct(Box::new(Framed::new(stream, SessionCodec)))
    }
}

/// Codec for the session connection when it runs over QUIC.
///
/// A websocket frames messages for us; a QUIC stream is a plain byte stream, so the framing has to
/// come from somewhere. bincode's encoding is self-delimiting - decoding consumes exactly one
/// message's worth of bytes and no more - so messages need no length prefix of their own, and a
/// message encoded elsewhere can be appended to the stream as it is. That is what lets one codec
/// encode both [`ClientMessage`]s and already-encoded bytes onto the same stream.
pub struct SessionCodec;

impl Decoder for SessionCodec {
    type Item = DaemonMessage;
    type Error = OperatorConnectionError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match bincode::decode_from_slice(&src[..], bincode::config::standard()) {
            Ok((message, read)) => {
                let _ = src.split_to(read);
                Ok(Some(message))
            }
            Err(bincode::error::DecodeError::UnexpectedEnd { .. }) => Ok(None),
            Err(error) => Err(OperatorConnectionError::DecodeError(error)),
        }
    }
}

impl Encoder<ClientMessage> for SessionCodec {
    type Error = OperatorConnectionError;

    fn encode(&mut self, item: ClientMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = bincode::encode_to_vec(&item, bincode::config::standard())?;
        dst.extend_from_slice(&encoded);

        Ok(())
    }
}

impl Encoder<Vec<u8>> for SessionCodec {
    type Error = OperatorConnectionError;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.extend_from_slice(&item);

        Ok(())
    }
}

impl Encoder<Bytes> for SessionCodec {
    type Error = OperatorConnectionError;

    fn encode(&mut self, item: Bytes, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.extend_from_slice(&item);

        Ok(())
    }
}

impl Stream for OperatorConnection {
    type Item = Result<DaemonMessage, OperatorConnectionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            OperatorConnection::WebSocket(stream) => {
                let item = match std::task::ready!(stream.poll_next_unpin(cx)) {
                    Some(Ok(Message::Binary(msg))) => {
                        match bincode::decode_from_slice(&msg, bincode::config::standard()) {
                            Ok((message, _)) => Some(Ok(message)),
                            Err(error) => Some(Err(OperatorConnectionError::DecodeError(error))),
                        }
                    }
                    // Operator only sends binary messages.
                    Some(Ok(unexpected)) => Some(Err(OperatorConnectionError::InvalidMessage(
                        Box::new(unexpected),
                    ))),
                    Some(Err(error)) => Some(Err(OperatorConnectionError::WsError(error.into()))),
                    None => None,
                };

                Poll::Ready(item)
            }
            OperatorConnection::Direct(stream) => stream.poll_next_unpin(cx),
        }
    }
}

/// Implements [`Sink`] for one of the item types the connection accepts.
///
/// The websocket puts each item in a frame of its own and needs [`ClientMessage`]s encoded on the
/// way in; the direct connection hands everything to [`SessionCodec`]. Both variants support every
/// item type, they just do not share a type to delegate to, so each one is written out per item
/// type rather than once.
macro_rules! sink_impl {
    ($item:ty, |$stream:ident, $item_name:ident| $websocket_send:expr) => {
        impl Sink<$item> for OperatorConnection {
            type Error = OperatorConnectionError;

            fn poll_close(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                match self.get_mut() {
                    OperatorConnection::WebSocket(stream) => {
                        stream.poll_close_unpin(cx).map_err(From::from)
                    }
                    OperatorConnection::Direct(stream) => {
                        <Framed<_, _> as SinkExt<$item>>::poll_close_unpin(stream, cx)
                    }
                }
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                match self.get_mut() {
                    OperatorConnection::WebSocket(stream) => {
                        stream.poll_flush_unpin(cx).map_err(From::from)
                    }
                    OperatorConnection::Direct(stream) => {
                        <Framed<_, _> as SinkExt<$item>>::poll_flush_unpin(stream, cx)
                    }
                }
            }

            fn poll_ready(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                match self.get_mut() {
                    OperatorConnection::WebSocket(stream) => {
                        stream.poll_ready_unpin(cx).map_err(From::from)
                    }
                    OperatorConnection::Direct(stream) => {
                        <Framed<_, _> as SinkExt<$item>>::poll_ready_unpin(stream, cx)
                    }
                }
            }

            fn start_send(self: Pin<&mut Self>, item: $item) -> Result<(), Self::Error> {
                match self.get_mut() {
                    OperatorConnection::WebSocket($stream) => {
                        let $item_name = item;
                        $websocket_send
                    }
                    OperatorConnection::Direct(stream) => stream.start_send_unpin(item),
                }
            }
        }
    };
}

sink_impl!(ClientMessage, |stream, item| {
    let encoded = bincode::encode_to_vec(&item, bincode::config::standard())?;
    stream
        .start_send_unpin(Message::Binary(encoded.into()))
        .map_err(From::from)
});

sink_impl!(Vec<u8>, |stream, item| {
    stream
        .start_send_unpin(Message::Binary(item.into()))
        .map_err(From::from)
});

sink_impl!(Bytes, |stream, item| {
    stream
        .start_send_unpin(Message::Binary(item))
        .map_err(From::from)
});

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
    /// The direct connection to the operator failed at the transport level.
    #[error("operator connection: {0}")]
    Io(#[from] io::Error),
}

impl From<tungstenite::Error> for OperatorConnectionError {
    fn from(error: tungstenite::Error) -> Self {
        Self::WsError(Box::new(error))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// The direct connection relies on bincode framing itself: a message appended to the stream as
    /// pre-encoded bytes has to decode back to exactly that message, leaving the bytes that follow
    /// it untouched. That is what makes the `Vec<u8>` and [`Bytes`] sinks safe to append blindly.
    #[test]
    fn encoded_messages_are_self_delimiting() {
        let mut codec = SessionCodec;
        let mut buffer = BytesMut::new();

        let version = semver::Version::new(1, 2, 3);

        Encoder::<ClientMessage>::encode(&mut codec, ClientMessage::Ping, &mut buffer).unwrap();
        let pre_encoded = bincode::encode_to_vec(
            ClientMessage::SwitchProtocolVersion(version.clone()),
            bincode::config::standard(),
        )
        .unwrap();
        Encoder::<Vec<u8>>::encode(&mut codec, pre_encoded, &mut buffer).unwrap();

        // Decoding is `DaemonMessage`-shaped, so read the messages back the way the operator's
        // codec would rather than through this one.
        let (first, read) = bincode::decode_from_slice::<ClientMessage, _>(
            &buffer[..],
            bincode::config::standard(),
        )
        .unwrap();
        assert!(matches!(first, ClientMessage::Ping));

        let (second, _) = bincode::decode_from_slice::<ClientMessage, _>(
            buffer.get(read..).expect("the first message was consumed"),
            bincode::config::standard(),
        )
        .unwrap();
        assert!(
            matches!(second, ClientMessage::SwitchProtocolVersion(decoded) if decoded == version),
        );
    }

    /// A partial message must leave the buffer alone and ask for more bytes, rather than erroring
    /// out and killing the session. QUIC delivers a stream, so a read landing mid-message is
    /// normal.
    #[test]
    fn partial_message_is_not_an_error() {
        let encoded =
            bincode::encode_to_vec(DaemonMessage::Pong, bincode::config::standard()).unwrap();
        let truncated = encoded
            .split_last()
            .expect("an encoded message is not empty")
            .1;
        let mut buffer = BytesMut::from(truncated);

        let decoded = SessionCodec.decode(&mut buffer).unwrap();

        assert!(decoded.is_none(), "a partial message should decode to None");
        assert_eq!(
            buffer.len(),
            encoded.len() - 1,
            "a partial message should not be consumed",
        );
    }
}
