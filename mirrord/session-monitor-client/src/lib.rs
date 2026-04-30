//! Cross-platform consumer for the per-session HTTP API exposed by mirrord-intproxy's session
//! monitor.
//!
//! - On unix the transport is a Unix domain socket at `~/.mirrord/sessions/{id}.sock`.
//! - On windows the transport is a named pipe at `\\.\pipe\mirrord-session-{id}`, with a sentinel
//!   marker file at `~/.mirrord/sessions/{id}.pipe` so filesystem-watcher-based discovery works the
//!   same way as on unix.
//!
//! HTTP is plain (no TLS) — confidentiality and authentication come from the OS-level access
//! control on the transport itself (`0o600` on the socket; restrictive DACL on the pipe).
//!
//! The client opens a fresh transport connection per request. There is no connection pool —
//! the per-session API is local and low-traffic, and per-request connections keep the
//! implementation small and the lifecycle of the SSE event stream simple.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
};

use bytes::Bytes;
use eventsource_stream::{Event as SseEvent, EventStreamError, Eventsource};
use futures::{Stream, StreamExt};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyDataStream, BodyExt, Empty};
use hyper_util::rt::TokioIo;
#[cfg(windows)]
pub use mirrord_session_monitor_protocol::pipe_name_for_session;
pub use mirrord_session_monitor_protocol::{ProcessInfo, SESSION_SENTINEL_EXTENSION, SessionInfo};
use thiserror::Error;

#[cfg(unix)]
type TransportStream = tokio::net::UnixStream;

#[cfg(windows)]
type TransportStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Returns `~/.mirrord/sessions`, the directory where session sentinel files (and on unix the
/// sockets themselves) live.
pub fn sessions_dir() -> Option<PathBuf> {
    home::home_dir().map(|home_dir| home_dir.join(".mirrord").join("sessions"))
}

/// Address of a single session's HTTP API.
///
/// On unix the [`Self::sentinel_path`] is the actual socket. On windows it is a marker file
/// next to which the named pipe runs; the transport uses [`Self::pipe_name`].
#[derive(Clone, Debug)]
pub struct SessionEndpoint {
    pub session_id: String,
    pub sentinel_path: PathBuf,
    #[cfg(windows)]
    pub pipe_name: String,
}

impl SessionEndpoint {
    /// Builds the endpoint for `session_id` rooted at `sessions_dir`. The sentinel file does
    /// not have to exist yet; this is also how the producer (intproxy) computes the path it
    /// will bind / write a marker at.
    pub fn for_session(session_id: &str, sessions_dir: &Path) -> Self {
        let sentinel_path = sessions_dir.join(format!("{session_id}.{SESSION_SENTINEL_EXTENSION}"));
        Self {
            session_id: session_id.to_owned(),
            sentinel_path,
            #[cfg(windows)]
            pipe_name: pipe_name_for_session(session_id),
        }
    }

    /// Reconstructs an endpoint from a sentinel path discovered by a filesystem watcher.
    /// Returns `None` if the path's stem is not valid UTF-8.
    pub fn from_sentinel(sentinel_path: &Path) -> Option<Self> {
        let session_id = sentinel_path.file_stem()?.to_str()?.to_owned();
        Some(Self {
            sentinel_path: sentinel_path.to_path_buf(),
            #[cfg(windows)]
            pipe_name: pipe_name_for_session(&session_id),
            session_id,
        })
    }

    async fn connect(&self) -> std::io::Result<TransportStream> {
        #[cfg(unix)]
        {
            tokio::net::UnixStream::connect(&self.sentinel_path).await
        }
        #[cfg(windows)]
        {
            connect_named_pipe(&self.pipe_name).await
        }
    }
}

/// Lists every session sentinel file in `sessions_dir` and returns its session id alongside
/// the [`SessionEndpoint`] it identifies.
pub fn session_endpoints(sessions_dir: &Path) -> Vec<(String, SessionEndpoint)> {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|extension| extension.to_str())
                == Some(SESSION_SENTINEL_EXTENSION))
            .then_some(path)
        })
        .filter_map(|path| {
            let endpoint = SessionEndpoint::from_sentinel(&path)?;
            Some((endpoint.session_id.clone(), endpoint))
        })
        .collect()
}

/// On windows, named pipes can be momentarily busy (`ERROR_PIPE_BUSY`) between the moment the
/// server pre-allocates a new instance and the next `connect()` returning. Retry briefly so
/// callers don't observe spurious failures.
#[cfg(windows)]
async fn connect_named_pipe(pipe_name: &str) -> std::io::Result<TransportStream> {
    use std::time::Duration;

    use tokio::net::windows::named_pipe::ClientOptions;
    use winapi::shared::winerror::ERROR_PIPE_BUSY;

    let mut attempts = 0;
    loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return Ok(client),
            Err(err) if attempts < 20 && err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("HTTP handshake failed: {0}")]
    Handshake(hyper::Error),
    #[error("HTTP request failed: {0}")]
    Request(hyper::Error),
    #[error("HTTP request build failed: {0}")]
    Build(http::Error),
    #[error("unexpected status {0} from session API")]
    BadStatus(StatusCode),
    #[error("body read failed: {0}")]
    Body(String),
    #[error("JSON deserialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// HTTP body emitted by the session API for streaming endpoints (currently `/events`). Maps
/// hyper body errors into `std::io::Error` so the public type doesn't leak hyper internals.
type SseByteResult = Result<Bytes, std::io::Error>;

/// Opaque pinned-boxed [`Stream`] of parsed Server-Sent Events. The error type is
/// [`std::io::Error`] for the same reason as [`SseByteResult`].
pub type EventStream = Pin<
    Box<dyn Stream<Item = Result<SseEvent, EventStreamError<std::io::Error>>> + Send + 'static>,
>;

/// Client for talking to a single session's HTTP API.
///
/// Each call opens a fresh connection over the underlying transport (Unix socket on unix,
/// named pipe on windows). The client itself is cheap to clone — it stores only the
/// [`SessionEndpoint`].
#[derive(Clone, Debug)]
pub struct SessionClient {
    endpoint: SessionEndpoint,
}

impl SessionClient {
    pub fn new(endpoint: SessionEndpoint) -> Self {
        Self { endpoint }
    }

    pub fn endpoint(&self) -> &SessionEndpoint {
        &self.endpoint
    }

    /// Opens a fresh hyper HTTP/1 connection over the session transport and returns the
    /// request sender. The connection runs on a spawned task; dropping the sender ends it.
    async fn open_h1_sender(
        &self,
    ) -> Result<hyper::client::conn::http1::SendRequest<Empty<Bytes>>, SessionError> {
        let stream = self.endpoint.connect().await?;
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(SessionError::Handshake)?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!(?err, "session monitor http1 connection ended");
            }
        });
        Ok(sender)
    }

    async fn send_request(
        &self,
        method: Method,
        path: &str,
        accept: Option<&str>,
    ) -> Result<http::Response<hyper::body::Incoming>, SessionError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("http://localhost{path}"))
            .header(http::header::HOST, "localhost");
        if let Some(accept) = accept {
            builder = builder.header(http::header::ACCEPT, accept);
        }
        let request = builder
            .body(Empty::<Bytes>::new())
            .map_err(SessionError::Build)?;
        let mut sender = self.open_h1_sender().await?;
        sender
            .send_request(request)
            .await
            .map_err(SessionError::Request)
    }

    pub async fn fetch_info(&self) -> Result<SessionInfo, SessionError> {
        let resp = self.send_request(Method::GET, "/info", None).await?;
        if !resp.status().is_success() {
            return Err(SessionError::BadStatus(resp.status()));
        }
        let body_bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| SessionError::Body(e.to_string()))?
            .to_bytes();
        Ok(serde_json::from_slice(&body_bytes)?)
    }

    pub async fn kill(&self) -> Result<(), SessionError> {
        let resp = self.send_request(Method::POST, "/kill", None).await?;
        if !resp.status().is_success() {
            return Err(SessionError::BadStatus(resp.status()));
        }
        Ok(())
    }

    pub async fn open_event_stream(&self) -> Result<EventStream, SessionError> {
        let resp = self
            .send_request(Method::GET, "/events", Some("text/event-stream"))
            .await?;
        if !resp.status().is_success() {
            return Err(SessionError::BadStatus(resp.status()));
        }
        let byte_stream = BodyDataStream::new(resp.into_body())
            .map(|frame_result| -> SseByteResult { frame_result.map_err(std::io::Error::other) });
        Ok(Box::pin(byte_stream.eventsource()))
    }
}

/// Convenience wrapper holding a connected client plus the session info fetched at connect
/// time. Useful when callers want both in one shot (the cli's session list does this).
pub struct SessionConnection {
    pub endpoint: SessionEndpoint,
    pub info: SessionInfo,
    pub client: SessionClient,
}

/// Connects to a session given its sentinel path (the `.sock` on unix or `.pipe` on windows),
/// then fetches `/info`. Returns the [`SessionConnection`] holding both.
pub async fn connect_to_session(sentinel_path: &Path) -> Result<SessionConnection, SessionError> {
    let endpoint = SessionEndpoint::from_sentinel(sentinel_path).ok_or_else(|| {
        SessionError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("sentinel path {sentinel_path:?} has no valid session id"),
        ))
    })?;
    let client = SessionClient::new(endpoint.clone());
    let info = client.fetch_info().await?;
    Ok(SessionConnection {
        endpoint,
        info,
        client,
    })
}
