#[derive(thiserror::Error, Debug)]
pub enum SessionsManagerClientError {
    #[error("SocketIO control plane error: {0}")]
    SocketIO(#[from] rust_socketio::Error),

    #[error("WebSocket data plane upgrade error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("JSON serialization or deserialization failed: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("The background control plane worker dropped before providing a payload")]
    ChannelDropped,

    #[error("Control plane initialization timed out")]
    Timeout,

    #[error("Cancellation token was signaled")]
    CancellationToken,

    #[error("Missing required env var: {0}")]
    VarError(#[from] std::env::VarError),
}
pub type Result<T> = std::result::Result<T, SessionsManagerClientError>;
