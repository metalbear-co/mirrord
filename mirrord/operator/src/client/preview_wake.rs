//! The CLI's side of `mirrord preview wake`: the `previews/{name}/wake` websocket. See
//! [`crate::wake`] for the protocol.

use futures::StreamExt;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message},
};

use super::preview_sync::UpgradedConnection;
use crate::wake::WakeStatus;

/// Errors of a preview wake connection.
#[derive(Debug, Error)]
pub enum PreviewWakeError {
    #[error("the wake connection to the operator failed: {0}")]
    WebSocket(#[from] tungstenite::Error),

    #[error("the operator closed the wake connection")]
    Closed,

    #[error("the operator sent a wake status that the CLI can't read: {0}")]
    Decode(#[from] serde_json::Error),
}

/// The `previews/{name}/wake` websocket of one preview session, which carries one [`WakeStatus`]
/// per text message. The operator holds the preview awake while it is open.
///
/// `S` is the stream under the websocket, which tests replace.
pub struct PreviewWakeConnection<S = UpgradedConnection> {
    socket: WebSocketStream<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> PreviewWakeConnection<S> {
    pub fn new(socket: WebSocketStream<S>) -> Self {
        Self { socket }
    }

    /// The next status from the operator. Reading also answers the operator's pings, which tell
    /// it that the CLI still waits.
    ///
    /// Cancel-safe: dropping the future before it returns loses no status.
    pub async fn next_status(&mut self) -> Result<WakeStatus, PreviewWakeError> {
        loop {
            let message = self.socket.next().await.ok_or(PreviewWakeError::Closed)??;
            match message {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Close(_) => return Err(PreviewWakeError::Closed),
                _ => continue,
            }
        }
    }
}
