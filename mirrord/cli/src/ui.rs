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
        Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, sse},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use eventsource_stream::Eventsource;
use futures::stream::StreamExt as _;
use mirrord_session_monitor_client::{
    SessionError, connect_to_session, session_socket_entries, sessions_dir,
};
use mirrord_session_monitor_protocol::SessionInfo;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rand::Rng;
#[cfg(not(debug_assertions))]
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    set_header::SetResponseHeaderLayer,
};
use tracing::{debug, error, info, warn};

use crate::{config::UiArgs, error::CliError};

const MAX_EVENTS_PER_SESSION: usize = 500;

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
    token: Option<String>,
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
    /// Stored for `read_session_token` to resolve per-session token files.
    #[allow(dead_code)]
    sessions_dir: PathBuf,
    kube_client: Option<kube::Client>,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Middleware that validates the request carries a valid auth token, either via the `mirrord_token`
/// cookie or the `?token=` query parameter.
async fn token_auth(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(query): Query<TokenQuery>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(cookie) = jar.get("mirrord_token")
        && cookie.value() == state.token
    {
        return next.run(request).await;
    }

    if let Some(token) = &query.token
        && token == &state.token
    {
        let mut response = next.run(request).await;
        let cookie = Cookie::build(("mirrord_token", state.token.clone()))
            .http_only(true)
            .same_site(SameSite::Strict)
            .path("/")
            .build();
        if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        return response;
    }

    StatusCode::UNAUTHORIZED.into_response()
}

/// Middleware that rejects requests with a Host header that is not localhost, 127.0.0.1, or [::1].
async fn host_validation(request: Request, next: Next) -> Response {
    if let Some(host) = request.headers().get(header::HOST)
        && let Ok(host_str) = host.to_str()
    {
        let host_without_port = host_str.split(':').next().unwrap_or(host_str);
        match host_without_port {
            "localhost" | "127.0.0.1" | "[::1]" | "::1" => {}
            _ => return StatusCode::FORBIDDEN.into_response(),
        }
    }
    next.run(request).await
}

fn localhost_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
            if let Ok(origin_str) = origin.to_str() {
                origin_str.starts_with("http://localhost")
                    || origin_str.starts_with("http://127.0.0.1")
                    || origin_str.starts_with("http://[::1]")
            } else {
                false
            }
        }))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::COOKIE, header::AUTHORIZATION])
        .allow_credentials(true)
}

/// Opens an SSE connection to a session's Unix socket /events endpoint and returns
/// a pinned `Eventsource` stream that yields parsed SSE events.
async fn open_sse_stream(
    client: &reqwest::Client,
    token: Option<&str>,
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
    let url = match token {
        Some(t) => format!("http://localhost/events?token={t}"),
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
async fn stream_session_events(
    session_id: String,
    client: reqwest::Client,
    token: Option<String>,
    state: Arc<AppState>,
) {
    let mut sse_stream = match open_sse_stream(&client, token.as_deref()).await {
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
    for (session_id, socket_path) in session_socket_entries(sessions_dir) {
        add_session(session_id, socket_path, state.clone()).await;
    }
}

async fn add_session(session_id: String, socket_path: PathBuf, state: Arc<AppState>) {
    if state.sessions.read().await.contains_key(&session_id) {
        return;
    }

    let connection = match connect_to_session(&socket_path).await {
        Ok(connection) => connection,
        Err(err) => {
            debug!(%session_id, ?err, "Could not fetch session info, removing stale socket");
            if let Err(err) = std::fs::remove_file(&socket_path) {
                warn!(%session_id, ?err, "Failed to remove stale socket");
            }
            return;
        }
    };

    let tracked = TrackedSession {
        socket_path: connection.socket_path.clone(),
        info: connection.info,
        events: Vec::new(),
        client: connection.client,
        token: connection.token,
    };

    let session_client = tracked.client.clone();
    let session_token = tracked.token.clone();
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

    tokio::spawn(stream_session_events(
        session_id,
        session_client,
        session_token,
        state,
    ));
}

async fn remove_session(session_id: &str, state: &AppState) {
    if state.sessions.write().await.remove(session_id).is_some() {
        let _ = state.notify_tx.send(SessionNotification::SessionRemoved {
            session_id: session_id.to_owned(),
        });
        info!(%session_id, "Session removed");
    }
}

/// Reads a session token from `~/.mirrord/sessions/<id>.token`, with path traversal protection.
/// Used when connecting to per-session APIs that require token auth.
#[allow(dead_code)]
fn read_session_token(sessions_dir: &std::path::Path, session_id: &str) -> Option<String> {
    if session_id.contains('/') || session_id.contains('\\') || session_id.contains("..") {
        return None;
    }

    let token_path = sessions_dir.join(format!("{session_id}.token"));

    // Canonicalize and verify the path is within sessions_dir
    let canonical = match token_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };
    let canonical_dir = match sessions_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };
    if !canonical.starts_with(&canonical_dir) {
        return None;
    }

    std::fs::read_to_string(&canonical).ok()
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
                session.token.clone(),
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

        use futures::stream::StreamExt as _;
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
    let (client, socket_path, session_token) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (
                session.client.clone(),
                session.socket_path.clone(),
                session.token.clone(),
            ),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let kill_url = match session_token.as_deref() {
        Some(token) => format!("http://localhost/kill?token={token}"),
        None => "http://localhost/kill".to_owned(),
    };
    match client.post(&kill_url).send().await {
        Ok(resp) => {
            let val: serde_json::Value = resp.json().await.unwrap_or_else(|err| {
                warn!(?err, "Failed to parse kill response body");
                serde_json::json!({"status": "ok"})
            });

            // Clean up socket file and remove session from tracking
            if let Err(err) = std::fs::remove_file(&socket_path) {
                warn!(%id, ?err, "Failed to remove session socket after kill");
            }
            remove_session(&id, &state).await;

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

/// Validates the WebSocket upgrade Origin header, rejecting non-localhost origins.
fn validate_ws_origin(headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN) {
        None => true, // No origin header is acceptable for non-browser clients
        Some(origin) => {
            if let Ok(origin_str) = origin.to_str() {
                origin_str.starts_with("http://localhost")
                    || origin_str.starts_with("http://127.0.0.1")
                    || origin_str.starts_with("http://[::1]")
            } else {
                false
            }
        }
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !validate_ws_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(|socket| ws_connection(socket, state))
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

/// Proxies GET /topology to the operator via k8s API server service proxy.
async fn proxy_topology(State(state): State<Arc<AppState>>) -> Response {
    let client = match &state.kube_client {
        Some(client) => client.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({"error": "kube client not available"})),
            )
                .into_response();
        }
    };

    let request = axum::http::Request::builder()
        .uri("/api/v1/namespaces/mirrord/services/mirrord-operator:3000/proxy/topology")
        .header(header::ACCEPT, "application/json")
        .body(Vec::new())
        .expect("static topology request should always be valid");

    match client.request::<serde_json::Value>(request).await {
        Ok(value) => axum::Json(value).into_response(),
        Err(err) => {
            warn!(?err, "Failed to proxy topology request");
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response()
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

async fn health() -> impl IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

fn build_router(state: Arc<AppState>) -> Router {
    let api_routes = Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/events", get(session_events_sse))
        .route("/sessions/{id}/kill", post(kill_session))
        .route("/topology", get(proxy_topology));

    let authenticated_routes = Router::new()
        .nest("/api", api_routes)
        .route("/ws", get(ws_handler))
        .fallback(static_handler)
        .layer(middleware::from_fn_with_state(state.clone(), token_auth));

    let csp_value = HeaderValue::from_static(
        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
         connect-src 'self' ws://localhost:* ws://127.0.0.1:* ws://[::1]:*; \
         img-src 'self' data:; object-src 'none'; frame-ancestors 'none'",
    );

    Router::new()
        .route("/health", get(health))
        .merge(authenticated_routes)
        .layer(middleware::from_fn(host_validation))
        .layer(localhost_cors())
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            csp_value,
        ))
        .with_state(state)
}

pub async fn ui_command(args: UiArgs) -> Result<(), CliError> {
    let sessions_dir = sessions_dir()
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?;

    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| CliError::UiError(format!("failed to create sessions directory: {e}")))?;

    let (notify_tx, _) = broadcast::channel::<SessionNotification>(256);

    // Generate auth token for this UI server instance
    let token_bytes: [u8; 32] = rand::rng().random();
    let token = hex::encode(token_bytes);

    // Try to create a kube client for topology proxy
    let kube_client = match kube::Client::try_default().await {
        Ok(client) => Some(client),
        Err(err) => {
            warn!(
                ?err,
                "Failed to create kube client, topology proxy will be unavailable"
            );
            None
        }
    };

    let state = Arc::new(AppState {
        sessions: RwLock::new(HashMap::new()),
        notify_tx,
        token: token.clone(),
        sessions_dir: sessions_dir.clone(),
        kube_client,
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
