use std::{
    convert::Infallible, fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration,
};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::{
    net::UnixListener,
    sync::{RwLock, broadcast::error::RecvError},
};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use tokio_util::sync::CancellationToken;

use super::{MonitorEvent, MonitorTx};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub process_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub target: String,
    pub started_at: String,
    pub mirrord_version: String,
    pub is_operator: bool,
    pub processes: Vec<ProcessInfo>,
    pub config: serde_json::Value,
}

struct AppState {
    session_info: RwLock<SessionInfo>,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
    token: String,
}

struct SocketCleanup {
    socket_path: PathBuf,
    token_path: PathBuf,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.socket_path) {
            tracing::warn!(?err, path = ?self.socket_path, "Failed to remove session socket");
        }
        if let Err(err) = fs::remove_file(&self.token_path) {
            tracing::warn!(?err, path = ?self.token_path, "Failed to remove session token file");
        }
    }
}

/// Generates a random 32-byte hex token for API authentication.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    hex::encode(bytes)
}

/// Parses the `mirrord_token` value from a raw `Cookie` header.
fn parse_cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| {
            let pair = pair.trim();
            pair.strip_prefix("mirrord_token=")
        })
}

/// Parses the `token` query parameter from the request URI.
fn parse_query_token(uri: &axum::http::Uri) -> Option<&str> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "token").then_some(value)
    })
}

/// Middleware that validates the authentication token on every request.
///
/// Checks the `mirrord_token` cookie first, then falls back to the `?token=` query parameter.
/// If the query parameter is valid, sets an `HttpOnly`, `SameSite=Strict` cookie for subsequent
/// requests.
async fn token_auth(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> impl IntoResponse {
    let expected = &state.token;

    // Check cookie first
    if let Some(cookie_token) = parse_cookie_token(request.headers())
        && cookie_token == expected
    {
        return next.run(request).await;
    }

    // Fall back to query param
    if let Some(query_token) = parse_query_token(request.uri())
        && query_token == expected
    {
        let mut response = next.run(request).await;
        let cookie_value = format!("mirrord_token={expected}; HttpOnly; SameSite=Strict; Path=/");
        response.headers_mut().insert(
            header::SET_COOKIE,
            cookie_value
                .parse()
                .expect("cookie value should be valid header"),
        );
        return response;
    }

    StatusCode::FORBIDDEN.into_response()
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let info = state.session_info.read().await;
    Json(info.clone())
}

async fn events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state
        .monitor_tx
        .subscribe()
        .expect("monitor_tx should be enabled when API server is running");

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data =
                serde_json::to_string(&event).expect("MonitorEvent serialization cannot fail");
            Some(Ok(Event::default().data(data)))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Cancels the API server's cancellation token, triggering graceful shutdown of the API server
/// only. The mirrord session lifecycle is managed separately by the intproxy.
async fn kill(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state.shutdown.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

/// Subscribes to monitor events and updates session_info.processes on
/// LayerConnected/LayerDisconnected events.
async fn update_processes_from_events(state: Arc<AppState>) {
    let mut rx = match state.monitor_tx.subscribe() {
        Some(rx) => rx,
        None => return,
    };

    loop {
        match rx.recv().await {
            Ok(MonitorEvent::LayerConnected { pid, process_name }) => {
                let mut info = state.session_info.write().await;
                if !info.processes.iter().any(|p| p.pid == pid) {
                    info.processes.push(ProcessInfo { pid, process_name });
                }
            }
            Ok(MonitorEvent::LayerDisconnected { pid }) => {
                let mut info = state.session_info.write().await;
                info.processes.retain(|p| p.pid != pid);
            }
            Ok(_) => {}
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(n, "Process tracker lagged, dropped events");
            }
            Err(RecvError::Closed) => break,
        }
    }
}

pub async fn start_api_server(
    session_info: SessionInfo,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let session_id = &session_info.session_id;
    let sessions_dir = home::home_dir()
        .ok_or("could not determine home directory")?
        .join(".mirrord")
        .join("sessions");

    fs::create_dir_all(&sessions_dir)?;
    fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700))?;

    let socket_path = sessions_dir.join(format!("{session_id}.sock"));
    let token_path = sessions_dir.join(format!("{session_id}.token"));

    let token = generate_token();

    // Write token file with restricted permissions so `mirrord ui` can read it
    fs::write(&token_path, &token)?;
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;

    // Remove stale socket if it exists
    if let Err(err) = fs::remove_file(&socket_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(?err, ?socket_path, "Failed to remove stale session socket");
    }

    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    let _cleanup = SocketCleanup {
        socket_path: socket_path.clone(),
        token_path,
    };

    let state = Arc::new(AppState {
        session_info: RwLock::new(session_info),
        monitor_tx,
        shutdown: shutdown.clone(),
        token: token.clone(),
    });

    // Spawn background task to update processes from monitor events
    tokio::spawn(update_processes_from_events(state.clone()));

    // Auth middleware applies to all routes except /health
    let authenticated_routes = Router::new()
        .route("/info", get(info))
        .route("/events", get(events))
        .route("/kill", post(kill))
        .layer(middleware::from_fn_with_state(state.clone(), token_auth));

    let app = Router::new()
        .route("/health", get(health))
        .merge(authenticated_routes)
        .with_state(state);

    tracing::info!(?socket_path, "Session monitor API server starting");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("Session monitor API server stopped");

    Ok(token)
}
