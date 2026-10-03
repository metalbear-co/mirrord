use eventsource_stream::EventStreamError;
use mirrord_operator_websocket::upgrade::ConnectError;
use mirrord_sessions_manager_protocol::SessionsManagerProtocolError;
use url::Url;

#[derive(thiserror::Error, Debug)]
pub enum SessionsManagerClientError {
    #[error("WebSocket data plane upgrade error: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("HTTP control plane request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WebSocket data-plane upgrade failed: {0}")]
    WebSocketUpgrade(#[from] ConnectError),

    #[error("URL is invalid: {0}")]
    Url(#[from] url::ParseError),
    #[error("HTTP control plane returned {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("HTTP control plane returned unexpected content type {0:?}")]
    InvalidContentType(Option<String>),
    #[error(transparent)]
    SseEventStream(#[from] EventStreamError<reqwest::Error>),
    #[error("sessions-manager {0} stream ended")]
    SseStreamEnded(&'static str),
    #[error(
        "sessions-manager base URL must be a hierarchical HTTP(S) URL without query or fragment"
    )]
    InvalidBaseUrl,
    #[error("sessions-manager base URL must be of schema http/s")]
    InvalidBaseUrlScheme(Url),
    #[error("MIRRORD_SESSIONS_MANAGER_URL is required for sessions-manager connections")]
    MissingSessionsManagerUrl,
    #[error("Missing serverless environment")]
    MissingConfigEnvironment,
    #[error("Missing serverless service")]
    MissingConfigService,
    #[error("Missing serverless replica id")]
    MissingAgentReplicaID,
    #[error(transparent)]
    ProtocolError(#[from] SessionsManagerProtocolError),
    #[error("authorization header is invalid")]
    InvalidAuthorization,
    #[error("sessions-manager shared secret is not a valid header value")]
    InvalidSharedSecret,
    #[error(
        "MIRRORD_SESSIONS_MANAGER_API_KEY must hold a MetalBear API key, which begins with \
         `metalbear_key_`"
    )]
    InvalidApiKey,
    #[error("MIRRORD_METALBEAR_CLOUD_BAGGAGE_SESSION is not a valid header value")]
    InvalidCloudBaggageSession,
    #[error(
        "MIRRORD_METALBEAR_CLOUD_URL must be an https URL, or http only on a loopback address \
         (any address in debug builds), got {0}"
    )]
    InsecureCloudUrl(Url),
    #[error(
        "the MetalBear API key was rejected; check MIRRORD_SESSIONS_MANAGER_API_KEY and the \
         cloud endpoint it is being presented to"
    )]
    ApiKeyRejected,
    #[error("MetalBear token exchange returned {0}")]
    TokenExchangeStatus(reqwest::StatusCode),
    /// Carries no source: the body is not echoed back, as it may hold a token.
    #[error("MetalBear token exchange response did not contain a `token`")]
    TokenExchangeMissingToken,
    #[error("MetalBear token exchange returned a token that is not a valid header value")]
    TokenNotHeaderValue,
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
    #[error("sessions-manager control-plane subscription is closed")]
    SubscriptionClosed,
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
            Self::HttpStatus(status) | Self::TokenExchangeStatus(status) => {
                *status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error()
            }
            Self::WebSocket(_)
            | Self::Http(_)
            | Self::SseEventStream(EventStreamError::Transport(_))
            | Self::SseStreamEnded(_)
            | Self::OperationTimeout
            | Self::WebSocketUpgradeTimeout
            | Self::WebSocketUpgrade(_) => true,
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
