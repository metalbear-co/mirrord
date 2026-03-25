//! # mirrord UI (Session Monitor)
//!
//! The `mirrord ui` command launches a web-based session monitor that aggregates events from all
//! active mirrord sessions. It watches `~/.mirrord/sessions/` for Unix socket files, connects
//! to each session's HTTP API, and serves a React frontend plus REST/SSE/WebSocket endpoints on
//! localhost.
//!
//! This replaces the Node.js aggregator that was previously in `monitor-frontend/server.ts`.

use std::{
    collections::{HashMap, hash_map::Entry},
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

const MAX_EVENTS_PER_SESSION: usize = 500;

#[derive(Embed)]
#[folder = "../../monitor-frontend/dist/"]
struct FrontendAssets;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionNotification {
    SessionAdded { session: TrackedSession },
    SessionRemoved { session_id: String },
}

#[derive(Clone, Debug)]
struct TrackedSession {
    socket_path: PathBuf,
    info: SessionInfo,
    events: Vec<serde_json::Value>,
}

impl Serialize for TrackedSession {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.info.serialize(serializer)
    }
}

struct AppState {
    sessions: RwLock<HashMap<String, TrackedSession>>,
    notify_tx: broadcast::Sender<SessionNotification>,
}

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

async fn fetch_session_info(
    socket_path: &std::path::Path,
) -> Result<SessionInfo, Box<dyn std::error::Error + Send + Sync>> {
    let (status, body) = unix_http_request(socket_path, "GET", "/info").await?;
    if !status.is_success() {
        return Err(format!("session info request returned {status}").into());
    }
    let info: SessionInfo = serde_json::from_slice(&body)?;
    Ok(info)
}

fn parse_sse_frames(leftover: &mut String, new_data: &[u8]) -> Vec<serde_json::Value> {
    leftover.push_str(&String::from_utf8_lossy(new_data));
    let mut values = Vec::new();
    while let Some(pos) = leftover.find("\n\n") {
        let chunk = leftover[..pos].to_owned();
        leftover.replace_range(..pos + 2, "");
        for line in chunk.lines() {
            if let Some(json_str) = line.strip_prefix("data:") {
                let json_str = json_str.trim();
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                    values.push(val);
                }
            }
        }
    }
    values
}

/// Opens an SSE stream to a session's Unix socket /events endpoint.
async fn open_sse_body(
    socket_path: &std::path::Path,
) -> Result<hyper::body::Incoming, Box<dyn std::error::Error + Send + Sync>> {
    let stream = UnixStream::connect(socket_path).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/events")
        .header("host", "localhost")
        .header("accept", "text/event-stream")
        .body(Full::<Bytes>::new(Bytes::new()))?;

    let resp = sender.send_request(req).await?;
    Ok(resp.into_body())
}

/// Connects to a session's SSE /events endpoint and buffers events into the shared state.
async fn stream_session_events(session_id: String, socket_path: PathBuf, state: Arc<AppState>) {
    let mut body = match open_sse_body(&socket_path).await {
        Ok(b) => b,
        Err(err) => {
            warn!(%session_id, ?err, "Failed to open SSE stream");
            return;
        }
    };

    let mut leftover = String::new();
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    let values = parse_sse_frames(&mut leftover, &data);
                    if !values.is_empty() {
                        let mut sessions = state.sessions.write().await;
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.events.extend(values);
                            if session.events.len() > MAX_EVENTS_PER_SESSION {
                                session
                                    .events
                                    .drain(..session.events.len() - MAX_EVENTS_PER_SESSION);
                            }
                        }
                    }
                }
            }
            Some(Err(_)) | None => break,
        }
    }

    info!(%session_id, "Session stream disconnected, removing");
    let _ = std::fs::remove_file(&socket_path);
    remove_session(&session_id, &state).await;
}

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

async fn add_session(session_id: String, socket_path: PathBuf, state: Arc<AppState>) {
    {
        let sessions = state.sessions.write().await;
        if sessions.contains_key(&session_id) {
            return;
        }
        // Drop write lock before the network call to avoid holding it during I/O.
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
        socket_path: socket_path.clone(),
        info,
        events: Vec::new(),
    };

    let notification = SessionNotification::SessionAdded {
        session: tracked.clone(),
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

    tokio::spawn(stream_session_events(session_id, socket_path, state));
}

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
    let (buffered_events, socket_path) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (session.events.clone(), session.socket_path.clone()),
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

        let mut body = match open_sse_body(&socket_path).await {
            Ok(b) => b,
            Err(err) => {
                warn!(?err, "Failed to open SSE stream for proxy");
                return;
            }
        };

        let mut leftover = String::new();
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        let values = parse_sse_frames(&mut leftover, &data);
                        for val in values {
                            let data_str = serde_json::to_string(&val)
                                .expect("SSE event serialization cannot fail");
                            if tx
                                .send(Ok(sse::Event::default().data(data_str)))
                                .await
                                .is_err()
                            {
                                return;
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
            let val: serde_json::Value = serde_json::from_slice(&body).unwrap_or_else(|err| {
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

    // Periodic rescan as fallback (filesystem watchers can miss events on macOS)
    let rescan_state = state.clone();
    let rescan_dir = sessions_dir.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            scan_existing_sessions(&rescan_dir, &rescan_state).await;
        }
    });

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

    app = app.fallback(static_handler);

    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| CliError::UiError(format!("failed to bind to {addr}: {e}")))?;

    let url = format!("http://127.0.0.1:{}", args.port);
    // Intentional CLI output: the URL is the primary user-facing information.
    eprintln!("mirrord session monitor: {url}");

    if let Err(err) = opener::open(&url) {
        warn!(?err, "Failed to open browser");
    }

    axum::serve(listener, app)
        .await
        .map_err(|e| CliError::UiError(format!("server error: {e}")))?;

    Ok(())
}
