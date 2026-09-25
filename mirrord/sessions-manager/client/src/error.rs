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
    #[error("Kubernetes request to the operator-hosted sessions-manager failed: {0}")]
    Kube(Box<kube::Error>),
    #[error(
        "the mirrord operator does not serve sessions-manager routes (HTTP {0}); \
         enable `sessionsManager.enabled` in the operator Helm chart"
    )]
    ServerlessSessionsManagerNotServed(reqwest::StatusCode),

    #[error("URL is invalid: {0}")]
    Url(#[from] url::ParseError),
    #[error("HTTP control plane returned {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("HTTP control plane returned unexpected content type {0:?}")]
    InvalidContentType(Option<String>),
    #[error(transparent)]
    SseEventStream(#[from] EventStreamError<reqwest::Error>),
    #[error(transparent)]
    KubeSseEventStream(Box<EventStreamError<kube::Error>>),
    #[error("failed to encode the control-plane subscription query: {0}")]
    QueryEncoding(#[from] serde_urlencoded::ser::Error),
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
    #[error(
        "serverless {field} {value:?} has no letters or digits, so it cannot be used as a \
         sessions-manager scope"
    )]
    UnsluggableScope { field: &'static str, value: String },
    #[error("Missing serverless replica id")]
    MissingAgentReplicaID,
    #[error(transparent)]
    ProtocolError(#[from] SessionsManagerProtocolError),
    #[error("authorization header is invalid")]
    InvalidAuthorization,
    #[error("sessions-manager shared secret is not a valid header value")]
    InvalidSharedSecret,
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
            Self::HttpStatus(status) => is_retryable_status(*status),
            // Statuses from kube-apiserver or the operator are retried like a standalone
            // sessions-manager's, so e.g. missing RBAC fails fast; anything else is a transport
            // failure on the way there.
            Self::Kube(error) => match error.as_ref() {
                kube::Error::Api(status) => {
                    reqwest::StatusCode::from_u16(status.code).is_ok_and(is_retryable_status)
                }
                _ => true,
            },
            Self::KubeSseEventStream(error) => {
                matches!(error.as_ref(), EventStreamError::Transport(_))
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

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

impl From<kube::Error> for SessionsManagerClientError {
    fn from(error: kube::Error) -> Self {
        Self::Kube(Box::new(error))
    }
}

impl From<EventStreamError<kube::Error>> for SessionsManagerClientError {
    fn from(error: EventStreamError<kube::Error>) -> Self {
        Self::KubeSseEventStream(Box::new(error))
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for SessionsManagerClientError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, SessionsManagerClientError>;
