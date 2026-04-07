//! # mirrord UI (Session Monitor)
//!
//! The `mirrord ui` command launches a web-based session monitor that aggregates events from all
//! active mirrord sessions. It watches `~/.mirrord/sessions/` for Unix socket files, connects
//! to each session's HTTP API, and serves a React frontend plus REST/SSE/WebSocket endpoints on
//! localhost.

use std::{
    collections::{HashMap, hash_map::Entry},
    convert::Infallible,
    net::{Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    extract::{
        Path, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, sse},
    routing::{get, post},
};
use eventsource_stream::Eventsource;
use futures::stream::StreamExt as _;
use mirrord_intproxy::session_monitor::api::SessionInfo;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rand::Rng;
#[cfg(not(debug_assertions))]
use rust_embed::Embed;
use serde::Serialize;
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    set_header::SetResponseHeaderLayer,
};
use tracing::{debug, error, info, warn};

use crate::{config::UiArgs, error::CliError};

const MAX_EVENTS_PER_SESSION: usize = 500;

/// Hostnames allowed in the `Host` header (DNS rebinding protection).
const ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// Origin prefixes allowed for WebSocket upgrades.
const ALLOWED_ORIGIN_PREFIXES: &[&str] =
    &["http://localhost:", "http://127.0.0.1:", "http://[::1]:"];

/// Errors from communicating with a per-session monitor API over its Unix socket.
#[derive(Debug, thiserror::Error)]
enum SessionError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("unexpected status {0} from session API")]
    BadStatus(reqwest::StatusCode),
}

#[cfg(not(debug_assertions))]
#[derive(Embed)]
#[folder = "../../monitor-frontend/dist/"]
struct FrontendAssets;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionNotification {
    SessionAdded { session: Box<TrackedSession> },
    SessionRemoved { session_id: String },
}

#[derive(Clone, Debug)]
struct TrackedSession {
    socket_path: PathBuf,
    info: SessionInfo,
    events: Vec<serde_json::Value>,
    client: reqwest::Client,
}

impl Serialize for TrackedSession {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.info.serialize(serializer)
    }
}

struct AppState {
    sessions: RwLock<HashMap<String, TrackedSession>>,
    notify_tx: broadcast::Sender<SessionNotification>,
    token: String,
    sessions_dir: PathBuf,
}

/// Generates a random 32-byte hex token for API authentication.
fn generate_token() -> String {
    let bytes: [u8; 16] = rand::rng().random();
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

    if let Some(cookie_token) = parse_cookie_token(request.headers())
        && cookie_token == expected
    {
        return next.run(request).await;
    }

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

/// Middleware that rejects requests whose `Host` header does not resolve to localhost.
async fn validate_host(request: Request, next: Next) -> impl IntoResponse {
    if let Some(host) = request.headers().get(header::HOST) {
        let host_str = host.to_str().unwrap_or("");
        let hostname = host_str.split(':').next().unwrap_or(host_str);
        if ALLOWED_HOSTS.contains(&hostname) {
            return next.run(request).await;
        }
    }
    StatusCode::FORBIDDEN.into_response()
}

/// Returns true if the `Origin` header value starts with an allowed localhost prefix.
fn is_allowed_origin(origin: &str) -> bool {
    ALLOWED_ORIGIN_PREFIXES
        .iter()
        .any(|prefix| origin.starts_with(prefix))
}

/// Reads the per-session authentication token from `~/.mirrord/sessions/<id>.token`.
///
/// Rejects session IDs containing path separators to prevent path traversal.
fn read_session_token(sessions_dir: &std::path::Path, session_id: &str) -> Option<String> {
    if session_id.contains('/') || session_id.contains('\\') || session_id.contains("..") {
        return None;
    }
    let token_path = sessions_dir.join(format!("{session_id}.token"));
    std::fs::read_to_string(token_path).ok()
}

/// Creates a reqwest client configured to connect via the given Unix socket.
fn unix_client(socket_path: &std::path::Path) -> Result<reqwest::Client, SessionError> {
    Ok(reqwest::Client::builder()
        .unix_socket(socket_path)
        .build()?)
}

async fn fetch_session_info(
    client: &reqwest::Client,
    session_token: Option<&str>,
) -> Result<SessionInfo, SessionError> {
    let url = match session_token {
        Some(token) => format!("http://localhost/info?token={token}"),
        None => "http://localhost/info".to_owned(),
    };
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(SessionError::BadStatus(resp.status()));
    }
    Ok(resp.json().await?)
}

/// Opens an SSE connection to a session's Unix socket /events endpoint and returns
/// a pinned `Eventsource` stream that yields parsed SSE events.
async fn open_sse_stream(
    client: &reqwest::Client,
    session_token: Option<&str>,
) -> Result<
    std::pin::Pin<
        Box<
            dyn futures::Stream<
                    Item = Result<
                        eventsource_stream::Event,
                        eventsource_stream::EventStreamError<reqwest::Error>,
                    >,
                > + Send,
        >,
    >,
    SessionError,
> {
    let url = match session_token {
        Some(token) => format!("http://localhost/events?token={token}"),
        None => "http://localhost/events".to_owned(),
    };
    let resp = client
        .get(&url)
        .header("accept", "text/event-stream")
        .send()
        .await?;
    let byte_stream = resp.bytes_stream();
    Ok(Box::pin(byte_stream.eventsource()))
}

/// Appends parsed SSE values to the session's event buffer, capping at
/// [`MAX_EVENTS_PER_SESSION`].
async fn buffer_session_events(session_id: &str, values: Vec<serde_json::Value>, state: &AppState) {
    let mut sessions = state.sessions.write().await;
    let Some(session) = sessions.get_mut(session_id) else {
        return;
    };
    session.events.extend(values);
    if session.events.len() > MAX_EVENTS_PER_SESSION {
        session
            .events
            .drain(..session.events.len() - MAX_EVENTS_PER_SESSION);
    }
}

/// Connects to a session's SSE /events endpoint and buffers events into the shared state.
async fn stream_session_events(session_id: String, client: reqwest::Client, state: Arc<AppState>) {
    let session_token = read_session_token(&state.sessions_dir, &session_id);
    let mut sse_stream = match open_sse_stream(&client, session_token.as_deref()).await {
        Ok(s) => s,
        Err(err) => {
            warn!(%session_id, ?err, "Failed to open SSE stream");
            return;
        }
    };

    while let Some(result) = sse_stream.next().await {
        let event = match result {
            Ok(e) => e,
            Err(err) => {
                debug!(%session_id, ?err, "SSE stream error");
                break;
            }
        };
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) {
            buffer_session_events(&session_id, vec![val], &state).await;
        }
    }

    info!(%session_id, "Session stream disconnected, removing");
    if let Some(session) = state.sessions.read().await.get(&session_id)
        && let Err(err) = std::fs::remove_file(&session.socket_path)
    {
        warn!(%session_id, ?err, "Failed to remove session socket");
    }
    remove_session(&session_id, &state).await;
}

async fn scan_existing_sessions(sessions_dir: &std::path::Path, state: &Arc<AppState>) {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("sock")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            let session_id = stem.to_owned();
            add_session(session_id, path, state.clone()).await;
        }
    }
}

async fn add_session(session_id: String, socket_path: PathBuf, state: Arc<AppState>) {
    if state.sessions.read().await.contains_key(&session_id) {
        return;
    }

    let client = match unix_client(&socket_path) {
        Ok(c) => c,
        Err(err) => {
            debug!(%session_id, ?err, "Could not create client for session socket");
            if let Err(err) = std::fs::remove_file(&socket_path) {
                warn!(%session_id, ?err, "Failed to remove stale socket");
            }
            return;
        }
    };

    let session_token = read_session_token(&state.sessions_dir, &session_id);
    let info = match fetch_session_info(&client, session_token.as_deref()).await {
        Ok(i) => i,
        Err(err) => {
            debug!(%session_id, ?err, "Could not fetch session info, removing stale socket");
            if let Err(err) = std::fs::remove_file(&socket_path) {
                warn!(%session_id, ?err, "Failed to remove stale socket");
            }
            return;
        }
    };

    let tracked = TrackedSession {
        socket_path: socket_path.clone(),
        info,
        events: Vec::new(),
        client,
    };

    let session_client = tracked.client.clone();
    let notification = SessionNotification::SessionAdded {
        session: Box::new(tracked.clone()),
    };

    {
        let mut sessions = state.sessions.write().await;
        if let Entry::Vacant(entry) = sessions.entry(session_id.clone()) {
            entry.insert(tracked);
        } else {
            return;
        }
    }

    let _ = state.notify_tx.send(notification);
    info!(%session_id, "Session added");

    tokio::spawn(stream_session_events(session_id, session_client, state));
}

async fn remove_session(session_id: &str, state: &AppState) {
    if state.sessions.write().await.remove(session_id).is_some() {
        let _ = state.notify_tx.send(SessionNotification::SessionRemoved {
            session_id: session_id.to_owned(),
        });
        info!(%session_id, "Session removed");
    }
}

async fn list_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let list: Vec<SessionInfo> = sessions.values().map(|s| s.info.clone()).collect();
    axum::Json(list)
}

async fn get_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let sessions = state.sessions.read().await;
    match sessions.get(&id) {
        Some(session) => axum::Json(session.info.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn session_events_sse(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let (buffered_events, client, session_token) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (
                session.events.clone(),
                session.client.clone(),
                read_session_token(&state.sessions_dir, &id),
            ),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let (tx, rx) = mpsc::channel::<Result<sse::Event, Infallible>>(256);

    tokio::spawn(async move {
        for event in buffered_events {
            let data =
                serde_json::to_string(&event).expect("buffered event serialization cannot fail");
            if tx.send(Ok(sse::Event::default().data(data))).await.is_err() {
                return;
            }
        }

        let mut sse_stream = match open_sse_stream(&client, session_token.as_deref()).await {
            Ok(s) => s,
            Err(err) => {
                warn!(?err, "Failed to open SSE stream for proxy");
                return;
            }
        };

        while let Some(result) = sse_stream.next().await {
            let event = match result {
                Ok(e) => e,
                Err(_) => break,
            };
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) {
                let data_str =
                    serde_json::to_string(&val).expect("SSE event serialization cannot fail");
                if tx
                    .send(Ok(sse::Event::default().data(data_str)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    });

    sse::Sse::new(ReceiverStream::new(rx))
        .keep_alive(
            sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

async fn kill_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let (client, session_token) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (
                session.client.clone(),
                read_session_token(&state.sessions_dir, &id),
            ),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let url = match session_token.as_deref() {
        Some(token) => format!("http://localhost/kill?token={token}"),
        None => "http://localhost/kill".to_owned(),
    };
    match client.post(&url).send().await {
        Ok(resp) => {
            let val: serde_json::Value = resp.json().await.unwrap_or_else(|err| {
                warn!(?err, "Failed to parse kill response body");
                serde_json::json!({"status": "ok"})
            });
            axum::Json(val).into_response()
        }
        Err(err) => {
            error!(?err, "Failed to proxy kill request");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn ws_handler(
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin_str = origin.to_str().unwrap_or("");
        if !is_allowed_origin(origin_str) {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.on_upgrade(|socket| ws_connection(socket, state))
        .into_response()
}

async fn ws_connection(mut socket: WebSocket, state: Arc<AppState>) {
    {
        let sessions = state.sessions.read().await;
        for session in sessions.values() {
            let notification = SessionNotification::SessionAdded {
                session: Box::new(session.clone()),
            };
            let msg = serde_json::to_string(&notification)
                .expect("notification serialization cannot fail");
            if socket.send(Message::Text(msg.into())).await.is_err() {
                return;
            }
        }
    }

    let mut rx = state.notify_tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(notification) => {
                let msg = serde_json::to_string(&notification)
                    .expect("notification serialization cannot fail");
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(n, "WebSocket client lagged, dropped messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn guess_mime(path: &str) -> &'static str {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

fn get_asset(path: &str) -> Option<Vec<u8>> {
    #[cfg(debug_assertions)]
    {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../monitor-frontend/dist/");
        std::fs::read(format!("{base}{path}")).ok()
    }
    #[cfg(not(debug_assertions))]
    {
        FrontendAssets::get(path).map(|f| f.data.to_vec())
    }
}

async fn static_handler(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    if let Some(data) = get_asset(path) {
        let mime = guess_mime(path);
        return ([(header::CONTENT_TYPE, mime)], data).into_response();
    }

    // SPA fallback
    match get_asset("index.html") {
        Some(data) => ([(header::CONTENT_TYPE, "text/html")], data).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Spawns a background task that rescans the sessions directory every 2 seconds.
/// Filesystem watchers can miss events on macOS, so this serves as a fallback.
#[cfg(target_os = "macos")]
fn start_periodic_rescan(sessions_dir: PathBuf, state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            scan_existing_sessions(&sessions_dir, &state).await;
        }
    });
}

/// Sets up a filesystem watcher on the sessions directory and spawns a background task
/// to handle socket file creation and removal events.
fn start_filesystem_watcher(
    sessions_dir: &std::path::Path,
    state: Arc<AppState>,
) -> Result<(), CliError> {
    let (watcher_tx, mut watcher_rx) = mpsc::channel::<notify::Event>(100);

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| match res {
            Ok(event) => {
                if let Err(err) = watcher_tx.blocking_send(event) {
                    tracing::warn!(%err, "Failed to send filesystem watcher event");
                }
            }
            Err(err) => {
                tracing::warn!(?err, "Filesystem watcher error");
            }
        })
        .map_err(|e| CliError::UiError(format!("failed to create file watcher: {e}")))?;

    watcher
        .watch(sessions_dir, RecursiveMode::NonRecursive)
        .map_err(|e| CliError::UiError(format!("failed to watch sessions directory: {e}")))?;

    tokio::spawn(async move {
        let _watcher = watcher;
        while let Some(event) = watcher_rx.recv().await {
            for path in &event.paths {
                if path.extension().and_then(|e| e.to_str()) != Some("sock") {
                    continue;
                }

                let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_owned(),
                    None => continue,
                };

                match event.kind {
                    EventKind::Create(_) => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        add_session(session_id, path.clone(), state.clone()).await;
                    }
                    EventKind::Remove(_) => {
                        remove_session(&session_id, &state).await;
                    }
                    _ => {}
                }
            }
        }
    });

    Ok(())
}

fn build_router(state: Arc<AppState>) -> Router {
    let api_routes = Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/events", get(session_events_sse))
        .route("/sessions/{id}/kill", post(kill_session));

    let authenticated_routes = Router::new()
        .nest("/api", api_routes)
        .route("/ws", get(ws_handler))
        .layer(middleware::from_fn_with_state(state.clone(), token_auth));

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            origin.to_str().ok().is_some_and(is_allowed_origin)
        }))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::COOKIE])
        .allow_credentials(true);

    let csp = "default-src 'self'; \
               script-src 'self'; \
               style-src 'self' 'unsafe-inline'; \
               connect-src 'self' ws://localhost:* wss://localhost:*";

    // Layers are applied bottom-up: host validation runs first (outermost), then CORS,
    // then security headers are appended to every response.
    Router::new()
        .merge(authenticated_routes)
        .with_state(state.clone())
        .fallback(static_handler)
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(csp),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(cors)
        .layer(middleware::from_fn(validate_host))
}

pub async fn ui_command(args: UiArgs) -> Result<(), CliError> {
    let sessions_dir = home::home_dir()
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?
        .join(".mirrord")
        .join("sessions");

    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| CliError::UiError(format!("failed to create sessions directory: {e}")))?;

    let (notify_tx, _) = broadcast::channel::<SessionNotification>(256);
    let token = generate_token();

    let state = Arc::new(AppState {
        sessions: RwLock::new(HashMap::new()),
        notify_tx,
        token: token.clone(),
        sessions_dir: sessions_dir.clone(),
    });

    scan_existing_sessions(&sessions_dir, &state).await;
    #[cfg(target_os = "macos")]
    start_periodic_rescan(sessions_dir.clone(), state.clone());
    start_filesystem_watcher(&sessions_dir, state.clone())?;

    let app = build_router(state);

    let addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| CliError::UiError(format!("failed to bind to {addr}: {e}")))?;

    let addr = listener
        .local_addr()
        .map_err(|e| CliError::UiError(format!("failed to get listener address: {e}")))?;
    let url = format!("http://{addr}?token={token}");
    eprintln!("mirrord session monitor: {url}");

    if let Err(err) = opener::open(&url) {
        warn!(?err, "Failed to open browser");
    }

    axum::serve(listener, app)
        .await
        .map_err(|e| CliError::UiError(format!("server error: {e}")))?;

    Ok(())
}
