//! # mirrord UI (Session Monitor)
//!
//! The `mirrord ui` command launches a web-based session monitor that aggregates events from all
//! active mirrord sessions. It watches `~/.mirrord/sessions/` for Unix socket files, connects
//! to each session's HTTP API, and serves a React frontend plus REST/SSE/WebSocket endpoints on
//! localhost.
//!
//! This replaces the Node.js aggregator that was previously in `monitor-frontend/server.ts`.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response, sse},
    routing::{get, post},
};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use mirrord_intproxy::session_monitor::api::SessionInfo;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rust_embed::Embed;
use serde::Serialize;
use tokio::{
    net::UnixStream,
    sync::{RwLock, broadcast, mpsc},
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

use crate::{config::UiArgs, error::CliError};

/// Embedded frontend assets from the monitor-frontend dist directory.
#[derive(Embed)]
#[folder = "../../monitor-frontend/dist/"]
struct FrontendAssets;

/// Notification broadcast when sessions are added or removed.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionNotification {
    SessionAdded { session: TrackedSession },
    SessionRemoved { session_id: String },
}

/// A tracked session with its info and buffered events.
#[derive(Clone, Debug)]
struct TrackedSession {
    session_id: String,
    socket_path: PathBuf,
    info: SessionInfo,
    events: Vec<serde_json::Value>,
}

impl Serialize for TrackedSession {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.info.serialize(serializer)
    }
}

/// Shared application state for the UI server.
struct AppState {
    sessions: RwLock<HashMap<String, TrackedSession>>,
    notify_tx: broadcast::Sender<SessionNotification>,
}

/// Makes an HTTP request to a Unix socket and returns the response body as bytes.
async fn unix_http_request(
    socket_path: &std::path::Path,
    method: &str,
    path: &str,
) -> Result<(StatusCode, Bytes), Box<dyn std::error::Error + Send + Sync>> {
    let stream = UnixStream::connect(socket_path).await?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        if let Err(err) = conn.await {
            debug!(?err, "Unix socket connection finished");
        }
    });

    let req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .body(Full::<Bytes>::new(Bytes::new()))?;

    let resp = sender.send_request(req).await?;
    let status = resp.status();
    let body = resp.into_body().collect().await?.to_bytes();
    Ok((status, body))
}

/// Fetches SessionInfo from a session's Unix socket.
async fn fetch_session_info(
    socket_path: &std::path::Path,
) -> Result<SessionInfo, Box<dyn std::error::Error + Send + Sync>> {
    let (_status, body) = unix_http_request(socket_path, "GET", "/info").await?;
    let info: SessionInfo = serde_json::from_slice(&body)?;
    Ok(info)
}

/// Connects to a session's SSE /events endpoint and buffers events into the shared state.
async fn stream_session_events(session_id: String, socket_path: PathBuf, state: Arc<AppState>) {
    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(err) => {
            warn!(%session_id, ?err, "Failed to connect for SSE streaming");
            return;
        }
    };
    let io = TokioIo::new(stream);

    let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
        Ok(pair) => pair,
        Err(err) => {
            warn!(%session_id, ?err, "Handshake failed for SSE");
            return;
        }
    };
    tokio::spawn(async move {
        if let Err(err) = conn.await {
            debug!(?err, "SSE connection finished");
        }
    });

    let req = match hyper::Request::builder()
        .method("GET")
        .uri("/events")
        .header("host", "localhost")
        .header("accept", "text/event-stream")
        .body(Full::<Bytes>::new(Bytes::new()))
    {
        Ok(r) => r,
        Err(err) => {
            warn!(%session_id, ?err, "Failed to build SSE request");
            return;
        }
    };

    let resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(err) => {
            warn!(%session_id, ?err, "SSE request failed");
            return;
        }
    };

    let mut body = resp.into_body();
    let mut leftover = String::new();

    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    leftover.push_str(&String::from_utf8_lossy(&data));
                    while let Some(pos) = leftover.find("\n\n") {
                        let chunk = leftover[..pos].to_owned();
                        leftover = leftover[pos + 2..].to_owned();
                        for line in chunk.lines() {
                            if let Some(json_str) = line.strip_prefix("data:") {
                                let json_str = json_str.trim();
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str)
                                {
                                    let mut sessions = state.sessions.write().await;
                                    if let Some(session) = sessions.get_mut(&session_id) {
                                        session.events.push(val);
                                        if session.events.len() > 500 {
                                            session.events.drain(..session.events.len() - 500);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some(Err(err)) => {
                debug!(%session_id, ?err, "SSE stream error");
                break;
            }
            None => {
                debug!(%session_id, "SSE stream ended");
                break;
            }
        }
    }

    // Session ended, clean up
    info!(%session_id, "Session stream disconnected, removing");
    let _ = std::fs::remove_file(&socket_path);
    remove_session(&session_id, &state).await;
}

/// Scans for existing socket files and adds them as sessions.
async fn scan_existing_sessions(sessions_dir: &std::path::Path, state: &Arc<AppState>) {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("sock") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let session_id = stem.to_owned();
                add_session(session_id, path, state.clone()).await;
            }
        }
    }
}

/// Adds a new session: fetches info, starts event streaming, and broadcasts notification.
async fn add_session(session_id: String, socket_path: PathBuf, state: Arc<AppState>) {
    {
        let sessions = state.sessions.read().await;
        if sessions.contains_key(&session_id) {
            return;
        }
    }

    let info = match fetch_session_info(&socket_path).await {
        Ok(i) => i,
        Err(err) => {
            debug!(%session_id, ?err, "Could not fetch session info, removing stale socket");
            let _ = std::fs::remove_file(&socket_path);
            return;
        }
    };

    let tracked = TrackedSession {
        session_id: session_id.clone(),
        socket_path: socket_path.clone(),
        info,
        events: Vec::new(),
    };

    let notification = SessionNotification::SessionAdded {
        session: tracked.clone(),
    };

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id.clone(), tracked);
    }

    let _ = state.notify_tx.send(notification);
    info!(%session_id, "Session added");

    tokio::spawn(stream_session_events(session_id, socket_path, state));
}

/// Removes a session and broadcasts a notification.
async fn remove_session(session_id: &str, state: &Arc<AppState>) {
    let removed = {
        let mut sessions = state.sessions.write().await;
        sessions.remove(session_id)
    };

    if removed.is_some() {
        let _ = state.notify_tx.send(SessionNotification::SessionRemoved {
            session_id: session_id.to_owned(),
        });
        info!(%session_id, "Session removed");
    }
}

/// Helper to build a JSON value for a session, including session_id at the top level.
fn session_to_json(session: &TrackedSession) -> serde_json::Value {
    let mut val = serde_json::to_value(&session.info).unwrap_or_default();
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "session_id".to_owned(),
            serde_json::Value::String(session.session_id.clone()),
        );
    }
    val
}

// --- HTTP Handlers ---

/// GET /api/sessions
async fn list_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let list: Vec<serde_json::Value> = sessions.values().map(session_to_json).collect();
    axum::Json(list)
}

/// GET /api/sessions/:id
async fn get_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let sessions = state.sessions.read().await;
    match sessions.get(&id) {
        Some(session) => axum::Json(session_to_json(session)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /api/sessions/:id/events - SSE proxy. Flushes buffered events first, then streams live.
async fn session_events_sse(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let (buffered_events, socket_path) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (session.events.clone(), session.socket_path.clone()),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let (tx, rx) = mpsc::channel::<Result<sse::Event, Infallible>>(256);

    tokio::spawn(async move {
        // Flush buffered events
        for event in buffered_events {
            let data = serde_json::to_string(&event).unwrap_or_default();
            if tx.send(Ok(sse::Event::default().data(data))).await.is_err() {
                return;
            }
        }

        // Open live SSE connection to the session socket
        let stream = match UnixStream::connect(&socket_path).await {
            Ok(s) => s,
            Err(err) => {
                warn!(?err, "Failed to connect to session socket for SSE proxy");
                return;
            }
        };
        let io = TokioIo::new(stream);

        let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
            Ok(pair) => pair,
            Err(err) => {
                warn!(?err, "Handshake failed for SSE proxy");
                return;
            }
        };
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let req = match hyper::Request::builder()
            .method("GET")
            .uri("/events")
            .header("host", "localhost")
            .header("accept", "text/event-stream")
            .body(Full::<Bytes>::new(Bytes::new()))
        {
            Ok(r) => r,
            Err(_) => return,
        };

        let resp = match sender.send_request(req).await {
            Ok(r) => r,
            Err(_) => return,
        };

        let mut body = resp.into_body();
        let mut leftover = String::new();

        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        leftover.push_str(&String::from_utf8_lossy(&data));
                        while let Some(pos) = leftover.find("\n\n") {
                            let chunk = leftover[..pos].to_owned();
                            leftover = leftover[pos + 2..].to_owned();
                            for line in chunk.lines() {
                                if let Some(json_str) = line.strip_prefix("data:") {
                                    let json_str = json_str.trim();
                                    if tx
                                        .send(Ok(sse::Event::default().data(json_str.to_owned())))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                Some(Err(_)) | None => break,
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

/// POST /api/sessions/:id/kill
async fn kill_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let socket_path = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => session.socket_path.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    match unix_http_request(&socket_path, "POST", "/kill").await {
        Ok((_status, body)) => {
            let val: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::json!({"status": "ok"}));
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

/// WebSocket handler: sends all current sessions on connect, then forwards notifications.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| ws_connection(socket, state))
}

async fn ws_connection(mut socket: WebSocket, state: Arc<AppState>) {
    {
        let sessions = state.sessions.read().await;
        for session in sessions.values() {
            let notification = SessionNotification::SessionAdded {
                session: session.clone(),
            };
            let msg = serde_json::to_string(&notification).unwrap_or_default();
            if socket.send(Message::Text(msg.into())).await.is_err() {
                return;
            }
        }
    }

    let mut rx = state.notify_tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(notification) => {
                let msg = serde_json::to_string(&notification).unwrap_or_default();
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

/// Returns the MIME type for a file path based on its extension.
fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("js") | Some("mjs") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Serves embedded static files with SPA fallback to index.html.
async fn static_handler(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    if let Some(file) = FrontendAssets::get(path) {
        let mime = guess_mime(path);
        return ([(header::CONTENT_TYPE, mime)], file.data.to_vec()).into_response();
    }

    // SPA fallback
    match FrontendAssets::get("index.html") {
        Some(file) => ([(header::CONTENT_TYPE, "text/html")], file.data.to_vec()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Entrypoint for the `mirrord ui` command.
pub async fn ui_command(args: UiArgs) -> Result<(), CliError> {
    let sessions_dir = home::home_dir()
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?
        .join(".mirrord")
        .join("sessions");

    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| CliError::UiError(format!("failed to create sessions directory: {e}")))?;

    let (notify_tx, _) = broadcast::channel::<SessionNotification>(256);

    let state = Arc::new(AppState {
        sessions: RwLock::new(HashMap::new()),
        notify_tx,
    });

    scan_existing_sessions(&sessions_dir, &state).await;

    // Set up filesystem watcher
    let watcher_state = state.clone();
    let (watcher_tx, mut watcher_rx) = mpsc::channel::<notify::Event>(100);

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = watcher_tx.blocking_send(event);
            }
        })
        .map_err(|e| CliError::UiError(format!("failed to create file watcher: {e}")))?;

    watcher
        .watch(&sessions_dir, RecursiveMode::NonRecursive)
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
                        add_session(session_id, path.clone(), watcher_state.clone()).await;
                    }
                    EventKind::Remove(_) => {
                        remove_session(&session_id, &watcher_state).await;
                    }
                    _ => {}
                }
            }
        }
    });

    let api_routes = Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/events", get(session_events_sse))
        .route("/sessions/{id}/kill", post(kill_session));

    let mut app = Router::new()
        .nest("/api", api_routes)
        .route("/ws", get(ws_handler))
        .with_state(state.clone());

    if args.dev {
        println!("Dev mode: not serving static files.");
        println!("Run `npm run dev` in monitor-frontend/ for the frontend.");
    } else {
        app = app.fallback(static_handler);
    }

    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| CliError::UiError(format!("failed to bind to {addr}: {e}")))?;

    let url = format!("http://127.0.0.1:{}", args.port);
    println!("mirrord session monitor: {url}");

    if !args.no_open && !args.dev {
        if let Err(err) = opener::open(&url) {
            warn!(?err, "Failed to open browser");
        }
    }

    axum::serve(listener, app)
        .await
        .map_err(|e| CliError::UiError(format!("server error: {e}")))?;

    Ok(())
}
