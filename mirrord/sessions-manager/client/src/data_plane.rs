//! Connects to the sessions-manager data plane and adapts its WebSocket transport to
//! [`mirrord_protocol_io::Connection`].
//!
//! Reads and writes are split to avoid blocking protocol responses behind an active read. The
//! writer task applies backpressure to application data while allowing WebSocket control responses
//! to bypass that queue.

use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    Sink, SinkExt, Stream, StreamExt,
    stream::{SplitSink, SplitStream},
};
use mirrord_protocol_io::{Connection, ProtocolEndpoint};
use mirrord_sessions_manager_protocol::ConnectionAssignment;
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{self, Message, client::IntoClientRequest, http::HeaderValue},
};
use tokio_util::{bytes::Bytes, sync::PollSender};
use url::Url;

use crate::error::SessionsManagerClientError;

const WEBSOCKET_UPGRADE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITER_COMMAND_BUFFER_CAPACITY: usize = 128;

/// Establishes the data-plane WebSocket described by a control-plane assignment.
///
/// The protocol validates the endpoint as an absolute-path relative URI; this client resolves it
/// against the configured control-plane URL and applies transport-specific checks. Assignments
/// currently assume that the data plane shares the control plane's origin, because the assignment
/// authorization is forwarded to the resolved endpoint. Supporting data planes on another origin
/// requires making that trusted origin explicit here and in the assignment protocol.
///
/// HTTP(S) assignment endpoints are converted to their WebSocket equivalents, and the assignment
/// authorization is forwarded as a sensitive request header.
pub(crate) async fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
    base_url: &Url,
    assignment: ConnectionAssignment,
) -> Result<Connection<E>, SessionsManagerClientError> {
    let scheme = match base_url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" | "wss" => base_url.scheme(),
        _ => {
            return Err(SessionsManagerClientError::InvalidBaseUrlScheme(
                base_url.clone(),
            ));
        }
    }
    .to_owned();
    let url = assignment.data_plane_endpoint.resolve(base_url, &scheme)?;
    let mut request = url.as_str().into_client_request()?;
    let mut authorization = HeaderValue::from_str(assignment.authorization.expose_secret())
        .map_err(|_| SessionsManagerClientError::InvalidAuthorization)?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(reqwest::header::AUTHORIZATION, authorization);

    let (stream, _) = tokio::time::timeout(WEBSOCKET_UPGRADE_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| SessionsManagerClientError::WebSocketUpgradeTimeout)??;
    Ok(Connection::from_channel(
        BinaryWebSocketConnection::<_, E>::new(stream),
    ))
}

/// Adapts a binary WebSocket to the stream-and-sink transport expected by
/// [`mirrord_protocol_io::Connection`].
///
/// The WebSocket is split because protocol handling may need to write while its parent task still
/// holds an active read poll. The writer half is therefore owned by a dedicated task. Ping and
/// close responses queued by tungstenite are explicitly flushed through that task.
struct BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    E: ProtocolEndpoint + Unpin,
{
    read_half: SplitStream<WebSocketStream<S>>,
    writer: WriterHandle,
    /// Keeps the stream pending until tungstenite's response to a peer close has been flushed.
    close_response_flush: Option<WriterAck>,
    marker: PhantomData<E>,
}

impl<S, E> BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    E: ProtocolEndpoint + Unpin + 'static,
{
    fn new(stream: WebSocketStream<S>) -> Self {
        let (sink, read_half) = stream.split();
        Self {
            read_half,
            writer: WriterHandle::new(sink),
            close_response_flush: None,
            marker: PhantomData,
        }
    }
}

impl<S, E> Stream for BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    E: ProtocolEndpoint + Unpin,
{
    type Item = Result<E::InMsg, WebSocketConnectionError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if let Some(ack) = this.close_response_flush.as_mut() {
            return match std::task::ready!(poll_writer_ack(ack, cx)) {
                Ok(()) => {
                    this.close_response_flush = None;
                    Poll::Ready(None)
                }
                Err(error) => {
                    this.close_response_flush = None;
                    Poll::Ready(Some(Err(error)))
                }
            };
        }

        loop {
            match std::task::ready!(this.read_half.poll_next_unpin(cx)) {
                Some(Ok(Message::Binary(msg))) => {
                    return Poll::Ready(Some(decode_binary_message::<E>(msg)));
                }
                Some(Ok(Message::Close(_))) => {
                    let mut ack = match this.writer.flush_peer_close() {
                        Ok(ack) => ack,
                        Err(error) => return Poll::Ready(Some(Err(error))),
                    };
                    match poll_writer_ack(&mut ack, cx) {
                        Poll::Ready(Ok(())) => return Poll::Ready(None),
                        Poll::Ready(Err(error)) => return Poll::Ready(Some(Err(error))),
                        Poll::Pending => {
                            this.close_response_flush = Some(ack);
                            return Poll::Pending;
                        }
                    }
                }
                Some(Ok(Message::Ping(_))) => {
                    if let Err(error) = this.writer.flush_automatic_response() {
                        return Poll::Ready(Some(Err(error)));
                    }
                }
                Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(message)) => {
                    return Poll::Ready(Some(Err(WebSocketConnectionError::InvalidMessage(
                        Box::new(message),
                    ))));
                }
                Some(Err(error)) => return Poll::Ready(Some(Err(error.into()))),
                None => return Poll::Ready(None),
            }
        }
    }
}

/// Decodes exactly one protocol message from a WebSocket frame.
///
/// Accepting trailing bytes could hide framing mismatches because each binary frame is expected to
/// contain one complete protocol message.
fn decode_binary_message<E: ProtocolEndpoint>(
    msg: Bytes,
) -> Result<E::InMsg, WebSocketConnectionError> {
    let (message, consumed) = bincode::decode_from_slice(&msg, bincode::config::standard())?;
    if consumed != msg.len() {
        return Err(WebSocketConnectionError::TrailingBytes {
            consumed,
            total: msg.len(),
        });
    }
    Ok(message)
}

impl<S, E> Sink<Vec<u8>> for BinaryWebSocketConnection<S, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    E: ProtocolEndpoint + Unpin,
{
    type Error = WebSocketConnectionError;

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_close(cx)
    }

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        self.writer.start_send(item.into())
    }
}

/// Presents the writer task as a [`Sink`] while keeping WebSocket control traffic responsive.
///
/// Application messages use a bounded channel through [`PollSender`], preserving `Sink`
/// backpressure. Automatic pong responses and peer-close responses bypass that queue so they
/// cannot be delayed by queued protocol data. Peer-close flushing is acknowledged because the
/// reader must not report EOF before the close response is written.
struct WriterHandle {
    commands: PollSender<WriterCommand>,
    automatic_flush_tx: mpsc::Sender<()>,
    peer_close_tx: mpsc::UnboundedSender<WriterAckSender>,
    operation: Option<PendingWriterOperation>,
    closed: bool,
    task: JoinHandle<()>,
}

/// An acknowledged `Sink` operation currently being polled by the caller.
enum PendingWriterOperation {
    Flush(WriterAck),
    Close(WriterAck),
}

type WriterAck = oneshot::Receiver<Result<(), WriterError>>;
type WriterAckSender = oneshot::Sender<Result<(), WriterError>>;

impl WriterHandle {
    fn new<S>(sink: SplitSink<WebSocketStream<S>, Message>) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (commands_tx, commands_rx) = mpsc::channel(WRITER_COMMAND_BUFFER_CAPACITY);
        let (automatic_flush_tx, automatic_flushes) = mpsc::channel(1);
        let (peer_close_tx, peer_close_requests) = mpsc::unbounded_channel();
        Self {
            commands: PollSender::new(commands_tx),
            automatic_flush_tx,
            peer_close_tx,
            operation: None,
            closed: false,
            task: tokio::spawn(run_writer(
                sink,
                commands_rx,
                automatic_flushes,
                peer_close_requests,
            )),
        }
    }

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketConnectionError>> {
        if self.closed {
            return Poll::Ready(Err(WebSocketConnectionError::WriterClosed));
        }

        Pin::new(&mut self.commands)
            .poll_reserve(cx)
            .map_err(|_| WebSocketConnectionError::WriterClosed)
    }

    fn start_send(&mut self, bytes: Bytes) -> Result<(), WebSocketConnectionError> {
        self.commands
            .send_item(WriterCommand::Send(bytes))
            .map_err(|_| WebSocketConnectionError::WriterClosed)
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketConnectionError>> {
        if self.closed {
            return Poll::Ready(Err(WebSocketConnectionError::WriterClosed));
        }

        if matches!(self.operation, Some(PendingWriterOperation::Close(_))) {
            return self.poll_operation(cx);
        }

        if self.operation.is_none() {
            self.commands.abort_send();
            std::task::ready!(self.poll_ready(cx)?);
            let (ack_tx, ack_rx) = oneshot::channel();
            self.commands
                .send_item(WriterCommand::Flush { ack: ack_tx })
                .map_err(|_| WebSocketConnectionError::WriterClosed)?;
            self.operation = Some(PendingWriterOperation::Flush(ack_rx));
        }

        self.poll_operation(cx)
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketConnectionError>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }
        if matches!(self.operation, Some(PendingWriterOperation::Flush(_))) {
            std::task::ready!(self.poll_operation(cx)?);
        }
        if self.operation.is_none() {
            self.commands.abort_send();
            std::task::ready!(self.poll_ready(cx)?);
            let (ack_tx, ack_rx) = oneshot::channel();
            self.commands
                .send_item(WriterCommand::Close { ack: ack_tx })
                .map_err(|_| WebSocketConnectionError::WriterClosed)?;
            self.operation = Some(PendingWriterOperation::Close(ack_rx));
        }

        self.poll_operation(cx)
    }

    /// Requests a flush for an automatic WebSocket response queued by tungstenite.
    ///
    /// A full channel means an equivalent flush is already pending, so duplicate requests can be
    /// safely coalesced.
    fn flush_automatic_response(&self) -> Result<(), WebSocketConnectionError> {
        match self.automatic_flush_tx.try_send(()) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(())) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(())) => {
                Err(WebSocketConnectionError::WriterClosed)
            }
        }
    }

    /// Requests an acknowledged flush of tungstenite's queued peer-close response.
    fn flush_peer_close(&self) -> Result<WriterAck, WebSocketConnectionError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.peer_close_tx
            .send(ack_tx)
            .map_err(|_| WebSocketConnectionError::WriterClosed)?;
        Ok(ack_rx)
    }

    fn poll_operation(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), WebSocketConnectionError>> {
        let (ack, closing) = match self.operation.as_mut().expect("writer operation is set") {
            PendingWriterOperation::Flush(ack) => (ack, false),
            PendingWriterOperation::Close(ack) => (ack, true),
        };
        let result = std::task::ready!(poll_writer_ack(ack, cx));
        self.operation = None;
        if closing || result.is_err() {
            self.closed = true;
            self.commands.close();
        }
        Poll::Ready(result)
    }
}

impl Drop for WriterHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Ordered application-side operations sent through the bounded writer queue.
enum WriterCommand {
    Send(Bytes),
    Flush { ack: WriterAckSender },
    Close { ack: WriterAckSender },
}

fn poll_writer_ack(
    ack: &mut WriterAck,
    cx: &mut Context<'_>,
) -> Poll<Result<(), WebSocketConnectionError>> {
    match std::task::ready!(Pin::new(ack).poll(cx)) {
        Ok(result) => Poll::Ready(result.map_err(Into::into)),
        Err(_) => Poll::Ready(Err(WebSocketConnectionError::WriterClosed)),
    }
}

/// Owns the WebSocket sink and serializes all writes and flushes.
///
/// Peer-close and automatic-response flushes are prioritized over application commands because
/// WebSocket liveness and shutdown must not depend on application queue capacity.
async fn run_writer<S>(
    mut sink: SplitSink<WebSocketStream<S>, Message>,
    mut commands: mpsc::Receiver<WriterCommand>,
    mut automatic_flushes: mpsc::Receiver<()>,
    mut peer_close_requests: mpsc::UnboundedReceiver<WriterAckSender>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            biased;
            Some(ack) = peer_close_requests.recv() => {
                let result = sink.flush().await.map_err(WriterError::from);
                let _ = ack.send(result);
                return;
            }
            Some(()) = automatic_flushes.recv() => {
                if let Err(error) = sink.flush().await {
                    tracing::warn!(%error, "sessions-manager data-plane writer failed");
                    return;
                }
            }
            Some(command) = commands.recv() => match command {
                WriterCommand::Send(bytes) => {
                    if let Err(error) = sink.feed(Message::Binary(bytes)).await {
                        tracing::warn!(%error, "sessions-manager data-plane writer failed");
                        return;
                    }
                }
                WriterCommand::Flush { ack } => {
                    let result = sink.flush().await.map_err(WriterError::from);
                    let failed = result.is_err();
                    let _ = ack.send(result);
                    if failed {
                        return;
                    }
                }
                WriterCommand::Close { ack } => {
                    let result = sink.close().await.map_err(WriterError::from);
                    let _ = ack.send(result);
                    return;
                }
            },
            else => {
                let _ = sink.close().await;
                return;
            }
        }
    }
}

#[derive(Error, Debug)]
enum WebSocketConnectionError {
    #[error("websocket writer is closed")]
    WriterClosed,
    #[error("websocket writer failed: {0}")]
    WriterFailed(#[from] WriterError),
    #[error("websocket read failed: {0}")]
    WebSocketRead(Box<tungstenite::Error>),
    #[error("bincode decode: {0}")]
    Decode(#[from] bincode::error::DecodeError),
    #[error("binary websocket message has trailing bytes: consumed {consumed} of {total}")]
    TrailingBytes { consumed: usize, total: usize },
    #[error("unexpected message: {0:?}")]
    InvalidMessage(Box<Message>),
}

impl From<tungstenite::Error> for WebSocketConnectionError {
    fn from(error: tungstenite::Error) -> Self {
        Self::WebSocketRead(Box::new(error))
    }
}

/// Preserves a writer-task error across an acknowledgment channel.
#[derive(Debug, Error)]
#[error(transparent)]
struct WriterError(Box<tungstenite::Error>);

impl From<tungstenite::Error> for WriterError {
    fn from(error: tungstenite::Error) -> Self {
        Self(Box::new(error))
    }
}
