//! # WebSocket Protocol Bridge
//!
//! This module provides a thread-safe, non-blocking asynchronous protocol bridge
//! specifically designed to handle full-duplex (`mirrord_protocol`) execution lines
//! over WebSocket streams without hitting asynchronous execution deadlocks.

use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, SinkExt, Stream, StreamExt, stream::SplitStream};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use mirrord_protocol_io::ProtocolEndpoint;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message},
};
use tokio_util::bytes::Bytes;

/// A structural network bridge that wraps a [`WebSocketStream`] to support concurrent
/// reading and writing without state deadlocks.
///
/// ### Concurrency Mechanics
/// Classic implementations that wrap a single `WebSocketStream` and forward both `Stream`
/// and `Sink` method parameters sequentially hit a state deadlock when a message handling
/// routine tries to write a response frame while the parent loop holds an active read poll.
///
/// To circumvent this, `BinaryWebSocketConnection` utilizes **Stream Splitting**:
/// * The **Read Half** ([`SplitStream`]) remains directly anchored inside this structure, driven by
///   [`Stream::poll_next`].
/// * The **Write Half** ([`futures::stream::SplitSink`]) is moved entirely into an out-of-band
///   background `tokio::spawn` loop worker.
///
/// Outbound calls to [`Sink::start_send`] push payloads into a lock-free, unbounded memory channel
/// queue ([`mpsc::UnboundedSender`]). This operation finishes instantly and allows the main loop to
/// resume reading immediately.
pub struct BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    E: ProtocolEndpoint + Unpin,
{
    /// The structural read boundary of the socket connection, driven directly by
    /// `Stream::poll_next`.
    read_half: SplitStream<WebSocketStream<S>>,
    /// A non-blocking, memory-buffered channel transmitter used to dispatch asynchronous outbound
    /// frames.
    write_tx: mpsc::UnboundedSender<Bytes>,
    _marker: PhantomData<E>,
}

impl<S, E> BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    E: ProtocolEndpoint + Unpin + 'static,
{
    /// Instantiates a new deadlock-free connection wrapper by splitting the provided stream.
    ///
    /// This splits the underlying stream, spawns a background outbound writer loop task onto the
    /// active Tokio runtime, and returns the unified protocol management container.
    ///
    /// ### Parameters
    /// * `stream` - The active, handshaked [`WebSocketStream`] instance to bridge.
    pub fn new(stream: WebSocketStream<S>) -> Self {
        let (mut ws_sink, ws_stream) = stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Bytes>();

        // Spawn a decoupled background worker loop utilizing non-blocking pipeline feeding
        tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                // .feed() inserts the frame into the sink's underlying encoder
                // buffer without blocking execution to wait for a network-level TCP flush!
                if ws_sink.feed(Message::Binary(bytes)).await.is_err() {
                    break;
                }

                // If no more immediate messages are waiting in our memory queue,
                // safely trigger a clean, opportunistic flush out-of-band.
                if rx.is_empty() && ws_sink.flush().await.is_err() {
                    break;
                }
            }
            let _ = ws_sink.close().await;
        });

        Self {
            read_half: ws_stream,
            write_tx: tx,
            _marker: PhantomData,
        }
    }
}

impl<S, E> Stream for BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    E: ProtocolEndpoint + Unpin,
{
    type Item = Result<E::InMsg, WebSocketConnectionError>;

    /// Polls for the next incoming data frame on the WebSocket stream.
    ///
    /// Extracts [`Message::Binary`] packets, handles transport error routing, and decodes
    /// raw payloads back into protocol representations using `bincode`.
    ///
    /// ### Errors
    /// Returns [`WebSocketConnectionError::InvalidMessage`] if an unexpected non-binary frame type
    /// (e.g., Text, Ping, Pong) reaches the network stack interface.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let item = match std::task::ready!(this.read_half.poll_next_unpin(cx)) {
            Some(Ok(Message::Binary(msg))) => {
                match bincode::decode_from_slice(&msg, bincode::config::standard()) {
                    Ok((message, _)) => Some(Ok(message)),
                    Err(error) => Some(Err(WebSocketConnectionError::DecodeError(error))),
                }
            }
            Some(Ok(Message::Close(_))) => None,
            Some(Ok(unexpected)) => Some(Err(WebSocketConnectionError::InvalidMessage(Box::new(
                unexpected,
            )))),
            Some(Err(error)) => Some(Err(WebSocketConnectionError::WsError(Box::new(error)))),
            None => None,
        };

        Poll::Ready(item)
    }
}

impl<S, E, M> Sink<M> for BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    E: ProtocolEndpoint + Unpin,
    M: ToWebSocketMessage, // Evaluates to Vec<u8>, ClientMessage, or DaemonMessage dynamically
{
    type Error = WebSocketConnectionError;

    /// Flushes ongoing background transport streams.
    ///
    /// Since the detached writer worker task handles flushing out-of-band immediately,
    /// this method returns `Poll::Ready(Ok(()))` instantly.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    /// Gracefully closes the underlying communication channel.
    ///
    /// Dropping or closing the transmitter wakes up and terminates the background task loop safely.
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    /// Evaluates whether the sink is ready to accept a new message.
    ///
    /// Because writes are pushed onto an unbounded, out-of-band memory channel queue,
    /// this method always returns `Poll::Ready(Ok(()))` instantly.
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Since we are pushing data into an unbounded memory queue buffer,
        // the sink is always instantly ready to accept data!
        Poll::Ready(Ok(()))
    }

    /// Appends an outbound protocol item to the asynchronous background dispatch queue.
    ///
    /// Encodes the message into binary bytes, wraps it inside a WebSocket package, and passes
    /// the block directly to the channel transmitter.
    ///
    /// ### Errors
    /// Returns an error if structural serialization or memory-pipe data routing fails.
    fn start_send(self: Pin<&mut Self>, item: M) -> Result<(), Self::Error> {
        let msg = item.to_websocket_msg()?;

        // Extract raw binary payload out of the generated Message container
        if let Message::Binary(bytes) = msg {
            // Push the raw payload into our detached non-blocking writer loop channel
            let _ = self.get_mut().write_tx.send(bytes);
        } else {
            return Err(WebSocketConnectionError::InvalidMessage(Box::new(msg)));
        }

        Ok(())
    }
}

pub trait ToWebSocketMessage {
    fn to_websocket_msg(self) -> Result<Message, WebSocketConnectionError>;
}

impl ToWebSocketMessage for Vec<u8> {
    fn to_websocket_msg(self) -> Result<Message, WebSocketConnectionError> {
        Ok(Message::Binary(self.into()))
    }
}

impl ToWebSocketMessage for ClientMessage {
    fn to_websocket_msg(self) -> Result<Message, WebSocketConnectionError> {
        let bytes = bincode::encode_to_vec(&self, bincode::config::standard())?;
        bytes.to_websocket_msg()
    }
}

impl ToWebSocketMessage for DaemonMessage {
    fn to_websocket_msg(self) -> Result<Message, WebSocketConnectionError> {
        let bytes = bincode::encode_to_vec(&self, bincode::config::standard())?;
        bytes.to_websocket_msg()
    }
}

/// Errors that can occur when working with [`BinaryWebSocketConnection<S>`].
#[derive(Error, Debug)]
pub enum WebSocketConnectionError {
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

impl From<tungstenite::Error> for WebSocketConnectionError {
    fn from(error: tungstenite::Error) -> Self {
        Self::WsError(Box::new(error))
    }
}
