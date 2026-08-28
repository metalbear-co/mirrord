use mirrord_sessions_manager_protocol::SessionsManagerProtocolError;
use url::Url;

#[derive(thiserror::Error, Debug)]
pub enum SessionsManagerClientError {
    #[error("WebSocket data plane upgrade error: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("HTTP control plane request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("URL is invalid: {0}")]
    Url(#[from] url::ParseError),
    #[error("HTTP control plane returned {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("HTTP control plane returned unexpected content type {0:?}")]
    InvalidContentType(Option<String>),
    #[error("HTTP/SSE stream failed: {0}")]
    Sse(String),
    #[error(
        "sessions-manager base URL must be a hierarchical HTTP(S) URL without query or fragment"
    )]
    InvalidBaseUrl,
    #[error("sessions-manager base URL must be of schema http/s")]
    InvalidBaseUrlScheme(Url),
    #[error("Invalid sessions-manager config: {0}")]
    InvalidConfig(String),
    #[error(transparent)]
    ProtocolError(#[from] SessionsManagerProtocolError),
    #[error("authorization header is invalid")]
    InvalidAuthorization,
    #[error("WebSocket request construction failed: {0}")]
    WebSocketRequest(#[from] tokio_tungstenite::tungstenite::http::Error),
    #[error("JSON serialization or deserialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("sessions-manager operation timed out")]
    OperationTimeout,
    #[error("WebSocket data-plane upgrade timed out")]
    WebSocketUpgradeTimeout,
    #[error("sessions-manager control-plane subscription was superseded")]
    Superseded,
    #[error("sessions-manager operation was cancelled")]
    Cancelled,
    #[error("Missing required env var: {0}")]
    VarError(#[from] std::env::VarError),
    #[error("control-plane task already shut down")]
    AlreadyShutdown,
    #[error("control-plane task panicked")]
    TaskPanicked,
    #[error("control-plane task was cancelled")]
    TaskCancelled,
}

impl SessionsManagerClientError {
    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::HttpStatus(status) => {
                *status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error()
            }
            Self::WebSocket(_)
            | Self::Http(_)
            | Self::Sse(_)
            | Self::OperationTimeout
            | Self::WebSocketUpgradeTimeout => true,
            _ => false,
        }
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for SessionsManagerClientError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, SessionsManagerClientError>;
