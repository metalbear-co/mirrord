//! # mirrord UI (Session Monitor)
//!
//! The `mirrord ui` command launches a web-based session monitor that aggregates events from all
//! active mirrord sessions. It watches `~/.mirrord/sessions/` for Unix socket files, connects
//! to each session's HTTP API, and serves a React frontend plus REST/SSE/WebSocket endpoints on
//! localhost.
//!
//! ## Browser extension handoff
//!
//! On startup, `mirrord ui` prints both the web UI URL and a `chrome-extension://` configure URL.
//! The extension id is pinned in the mirrord-browser manifest's `"key"` field, which produces a
//! deterministic id when loaded unpacked or published to the Chrome Web Store.

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
use kube::{Api, Client, api::ListParams, runtime::watcher};
use mirrord_operator::crd::session::MirrordSession;
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
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::{debug, error, info, warn};

use crate::{config::UiArgs, error::CliError};

const MAX_EVENTS_PER_SESSION: usize = 500;

#[cfg(not(debug_assertions))]
#[derive(Embed)]
#[folder = "../../packages/monitor/dist/"]
struct FrontendAssets;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionNotification {
    SessionAdded {
        session: Box<TrackedSession>,
    },
    SessionRemoved {
        session_id: String,
    },
    OperatorSessionAdded {
        session: Box<OperatorSessionSummary>,
    },
    OperatorSessionRemoved {
        name: String,
    },
    OperatorSessionUpdated {
        session: Box<OperatorSessionSummary>,
    },
}

/// Wire-format summary of an operator-side session that the browser extension
/// and local UI tab can render. Derived from `MirrordClusterSession`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorSessionSummary {
    /// Kubernetes object name of the underlying `MirrordClusterSession` CR.
    pub name: String,
    /// Session key (grouping identity). `None` if the session was started by an
    /// older CLI that didn't send a key, or by an operator that doesn't persist it yet.
    pub key: Option<String>,
    pub namespace: String,
    pub owner: Option<OperatorSessionOwner>,
    pub target: Option<OperatorSessionTarget>,
    pub created_at: Option<String>,
    /// Subset of the dev's `http_filter` needed by external consumers (the
    /// browser extension parses `header_filter` to build a matching DNR rule).
    /// `None` if the session was started by an older CLI or operator that
    /// didn't persist it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<OperatorSessionHttpFilter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorSessionHttpFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_filter: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorSessionOwner {
    pub username: String,
    pub k8s_username: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorSessionTarget {
    pub kind: String,
    pub name: String,
    pub container: String,
}

impl OperatorSessionSummary {
    /// Builds a summary from the [`MirrordSession`] served by the operator's API
    /// aggregation endpoint. The metadata carries the namespace and creation
    /// timestamp; the spec carries the joinable identity (key, target, filter).
    fn from_cr(cr: &MirrordSession) -> Option<Self> {
        let name = cr.metadata.name.clone()?;
        let namespace = cr.metadata.namespace.clone()?;
        let spec = &cr.spec;
        Some(Self {
            name,
            key: Some(spec.key.clone()),
            namespace,
            owner: Some(OperatorSessionOwner {
                username: spec.owner.username.clone(),
                k8s_username: spec.owner.k8s_username.clone(),
            }),
            target: spec.target.as_ref().map(|t| OperatorSessionTarget {
                kind: t.kind.clone(),
                name: t.name.clone(),
                container: t.container.clone(),
            }),
            created_at: cr
                .metadata
                .creation_timestamp
                .as_ref()
                .map(|ts| ts.0.to_string()),
            http_filter: spec
                .http_filter
                .as_ref()
                .map(|f| OperatorSessionHttpFilter {
                    header_filter: f.header_filter.clone(),
                }),
        })
    }
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
    operator_sessions: RwLock<std::collections::BTreeMap<String, OperatorSessionSummary>>,
    operator_watch_status: RwLock<OperatorWatchStatus>,
    notify_tx: broadcast::Sender<SessionNotification>,
    token: String,
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperatorWatchStatus {
    #[default]
    NotStarted,
    Watching,
    Error {
        message: String,
    },
    Unavailable {
        reason: String,
    },
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

/// Opens an SSE connection to a session's Unix socket /events endpoint and returns
/// a pinned `Eventsource` stream that yields parsed SSE events.
async fn open_sse_stream(
    client: &reqwest::Client,
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
    let resp = client
        .get("http://localhost/events")
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
    let mut sse_stream = match open_sse_stream(&client).await {
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
    let (buffered_events, client) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (session.events.clone(), session.client.clone()),
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

        let mut sse_stream = match open_sse_stream(&client).await {
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

#[derive(Serialize)]
struct OperatorSessionsResponse {
    /// Sessions grouped by key. The key is the map key; value is the list
    /// of sessions under that key. Sessions whose `spec.key` is `None` are
    /// collected under a sentinel key `""` (empty string).
    by_key: std::collections::BTreeMap<String, Vec<OperatorSessionSummary>>,
    /// Flat list of all sessions, for clients that prefer not to consume
    /// the grouped view.
    sessions: Vec<OperatorSessionSummary>,
    /// Current watch status (for UI diagnostics).
    watch_status: OperatorWatchStatus,
}

async fn list_operator_sessions(
    State(state): State<Arc<AppState>>,
) -> axum::Json<OperatorSessionsResponse> {
    let map = state.operator_sessions.read().await;
    let watch_status = state.operator_watch_status.read().await.clone();

    let mut by_key: std::collections::BTreeMap<String, Vec<OperatorSessionSummary>> =
        std::collections::BTreeMap::new();
    let sessions: Vec<OperatorSessionSummary> = map.values().cloned().collect();
    for s in &sessions {
        let k = s.key.clone().unwrap_or_default();
        by_key.entry(k).or_default().push(s.clone());
    }

    axum::Json(OperatorSessionsResponse {
        by_key,
        sessions,
        watch_status,
    })
}

async fn kill_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let (client, socket_path) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (session.client.clone(), session.socket_path.clone()),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    match client.post("http://localhost/kill").send().await {
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

fn guess_mime(path: &str) -> &'static str {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

fn get_asset(path: &str) -> Option<Vec<u8>> {
    #[cfg(debug_assertions)]
    {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/monitor/dist/");
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

/// Starts a tokio task that watches `MirrordClusterSession` CRs via `kube::Api` and
/// updates `AppState::operator_sessions`. Broadcasts `OperatorSession*` events on
/// `notify_tx`. If the cluster isn't reachable or the CRD isn't installed, logs a
/// warning and sets `operator_watch_status` to `Unavailable { reason }`. The `mirrord
/// ui` daemon continues running for local sessions either way.
fn start_operator_watcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let client = match Client::try_default().await {
            Ok(client) => client,
            Err(err) => {
                let reason = format!("kube client init failed: {err}");
                warn!("{reason}");
                *state.operator_watch_status.write().await =
                    OperatorWatchStatus::Unavailable { reason };
                return;
            }
        };

        // Watch the namespaced `MirrordSession` projection instead of the
        // cluster-scoped `MirrordClusterSession`. `Api::all` still spans every
        // namespace for the list/watch; callers only see sessions their RBAC
        // permits.
        let api: Api<MirrordSession> = Api::all(client);

        // Probe the CRD with a single list; if this fails because the CRD isn't
        // installed or access is denied, surface a clean "unavailable" status
        // instead of retrying forever.
        if let Err(err) = api.list(&ListParams::default().limit(1)).await {
            let reason = format!("operator CRD not available: {err}");
            warn!("{reason}");
            *state.operator_watch_status.write().await =
                OperatorWatchStatus::Unavailable { reason };
            return;
        }

        *state.operator_watch_status.write().await = OperatorWatchStatus::Watching;

        let mut stream = watcher(api, watcher::Config::default()).boxed();
        while let Some(ev) = stream.next().await {
            match ev {
                Ok(watcher::Event::Apply(cr)) => {
                    if let Some(summary) = OperatorSessionSummary::from_cr(&cr) {
                        let key = summary.name.clone();
                        let is_new = {
                            let mut map = state.operator_sessions.write().await;
                            let prior = map.insert(key.clone(), summary.clone());
                            prior.is_none()
                        };
                        let _ = state.notify_tx.send(if is_new {
                            SessionNotification::OperatorSessionAdded {
                                session: Box::new(summary),
                            }
                        } else {
                            SessionNotification::OperatorSessionUpdated {
                                session: Box::new(summary),
                            }
                        });
                    }
                }
                Ok(watcher::Event::Delete(cr)) => {
                    if let Some(name) = cr.metadata.name {
                        state.operator_sessions.write().await.remove(&name);
                        let _ = state
                            .notify_tx
                            .send(SessionNotification::OperatorSessionRemoved { name });
                    }
                }
                Ok(watcher::Event::Init)
                | Ok(watcher::Event::InitApply(_))
                | Ok(watcher::Event::InitDone) => {
                    // watcher relist lifecycle; no action needed.
                }
                Err(err) => {
                    let reason = format!("watcher error: {err}");
                    warn!("{reason}");
                    *state.operator_watch_status.write().await =
                        OperatorWatchStatus::Error { message: reason };
                }
            }
        }

        warn!("operator session watcher stream ended unexpectedly");
    });
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
        .route("/operator-sessions", get(list_operator_sessions));

    let authenticated_routes = Router::new()
        .nest("/api", api_routes)
        .route("/ws", get(ws_handler))
        .fallback(static_handler)
        .layer(middleware::from_fn_with_state(state.clone(), token_auth));

    // posthog-js lazy-loads its session recorder bundle from the api host at runtime, so the
    // PostHog origin must be listed in both `script-src` (for the recorder bundle) and
    // `connect-src` (for `/e/` event capture and `/s/` session replay ingest). Without these,
    // telemetry is silently dropped by the browser's CSP enforcement.
    let csp_value = HeaderValue::from_static(
        "default-src 'self'; script-src 'self' https://hog.metalbear.com; \
         style-src 'self' 'unsafe-inline'; \
         connect-src 'self' https://hog.metalbear.com \
         ws://localhost:* ws://127.0.0.1:* ws://[::1]:*; \
         img-src 'self' data:; object-src 'none'; frame-ancestors 'none'",
    );

    Router::new()
        .route("/health", get(health))
        .merge(authenticated_routes)
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

    let state = Arc::new(AppState {
        sessions: RwLock::new(HashMap::new()),
        operator_sessions: RwLock::new(std::collections::BTreeMap::new()),
        operator_watch_status: RwLock::new(OperatorWatchStatus::NotStarted),
        notify_tx,
        token: token.clone(),
    });

    scan_existing_sessions(&sessions_dir, &state).await;
    #[cfg(target_os = "macos")]
    start_periodic_rescan(sessions_dir.clone(), state.clone());
    start_filesystem_watcher(&sessions_dir, state.clone())?;
    start_operator_watcher(state.clone());

    let app = build_router(state);

    let addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| CliError::UiError(format!("failed to bind to {addr}: {e}")))?;

    let addr = listener
        .local_addr()
        .map_err(|e| CliError::UiError(format!("failed to get listener address: {e}")))?;
    let url = format!("http://{addr}?token={token}");
    let extension_url = build_extension_configure_url(&addr, &token);

    eprintln!();
    eprintln!("  mirrord session monitor");
    eprintln!("    Web UI:             {url}");
    eprintln!("    Browser extension:  {extension_url}");
    eprintln!();

    if let Err(err) = opener::open(&url) {
        warn!(?err, "Failed to open browser");
    }

    axum::serve(listener, app)
        .await
        .map_err(|e| CliError::UiError(format!("server error: {e}")))?;

    Ok(())
}

/// Chrome extension id produced by the `"key"` field in the mirrord-browser
/// manifest. Stable across dev (unpacked) and production (Chrome Web Store)
/// installs, so the CLI can hand a clickable configure URL to the user.
const MIRRORD_EXTENSION_ID: &str = "bijejadnnfgjkfdocgocklekjhnhkhkf";

/// Build a `chrome-extension://…/pages/configure.html?backend=…&token=…` URL
/// that configures the browser extension to talk to this UI server.
fn build_extension_configure_url(addr: &SocketAddr, token: &str) -> String {
    let backend = format!("http://{addr}");
    let backend_encoded: String =
        url::form_urlencoded::byte_serialize(backend.as_bytes()).collect();
    format!(
        "chrome-extension://{id}/pages/configure.html?backend={backend_encoded}&token={token}",
        id = MIRRORD_EXTENSION_ID
    )
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;

    use super::*;

    const TEST_TOKEN: &str = "test-token-1234567890abcdef";

    fn test_state() -> Arc<AppState> {
        let (notify_tx, _) = broadcast::channel(16);
        Arc::new(AppState {
            sessions: RwLock::new(HashMap::new()),
            operator_sessions: RwLock::new(std::collections::BTreeMap::new()),
            operator_watch_status: RwLock::new(OperatorWatchStatus::default()),
            notify_tx,
            token: TEST_TOKEN.to_owned(),
        })
    }

    fn req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn status_of(req: Request<Body>) -> StatusCode {
        build_router(test_state())
            .oneshot(req)
            .await
            .unwrap()
            .status()
    }

    /// `/health` is intentionally outside the auth middleware so k8s probes can hit it.
    #[tokio::test]
    async fn health_endpoint_does_not_require_token() {
        assert_eq!(status_of(req("/health")).await, StatusCode::OK);
    }

    /// Without a token, every API request should be rejected. This is the core protection
    /// against malicious local processes and CSRF from other browser tabs.
    #[tokio::test]
    async fn api_without_token_returns_unauthorized() {
        assert_eq!(
            status_of(req("/api/sessions")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// A request with the wrong token must also be rejected (no timing oracle, just a
    /// constant-time string compare via `==` is fine because the token is high-entropy).
    #[tokio::test]
    async fn api_with_wrong_token_returns_unauthorized() {
        assert_eq!(
            status_of(req("/api/sessions?token=wrong")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// First request with a valid `?token=` query param should both succeed AND set the
    /// `mirrord_token` cookie so subsequent requests can authenticate via cookie alone.
    #[tokio::test]
    async fn api_with_valid_token_query_param_sets_cookie() {
        let response = build_router(test_state())
            .oneshot(req(&format!("/api/sessions?token={TEST_TOKEN}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("auth middleware should set the mirrord_token cookie");
        let cookie_str = cookie.to_str().unwrap();
        assert!(cookie_str.contains(&format!("mirrord_token={TEST_TOKEN}")));
        assert!(
            cookie_str.contains("HttpOnly"),
            "cookie must be HttpOnly to prevent XSS exfiltration"
        );
        assert!(
            cookie_str.contains("SameSite=Strict"),
            "cookie must be SameSite=Strict to prevent CSRF"
        );
    }

    /// Once the cookie is set, requests should authenticate via cookie without needing
    /// the query param. This is what the React frontend does after the initial page load.
    #[tokio::test]
    async fn api_with_valid_cookie_succeeds() {
        let request = Request::builder()
            .uri("/api/sessions")
            .header(header::COOKIE, format!("mirrord_token={TEST_TOKEN}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::OK);
    }

    /// A wrong cookie must be rejected. Without this, an attacker who guesses or steals
    /// a stale token could impersonate the user.
    #[tokio::test]
    async fn api_with_wrong_cookie_returns_unauthorized() {
        let request = Request::builder()
            .uri("/api/sessions")
            .header(header::COOKIE, "mirrord_token=wrong")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::UNAUTHORIZED);
    }

    /// All authenticated responses should carry the security headers from the RFC.
    /// These protect against clickjacking, MIME sniffing, referrer leaks, and XSS.
    #[tokio::test]
    async fn responses_include_security_headers() {
        let response = build_router(test_state())
            .oneshot(req(&format!("/api/sessions?token={TEST_TOKEN}")))
            .await
            .unwrap();

        let headers = response.headers();
        assert_eq!(headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
        assert_eq!(
            headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        assert_eq!(headers.get(header::REFERRER_POLICY).unwrap(), "no-referrer");

        // Per the RFC, CSP must restrict scripts to 'self' (no inline scripts).
        let csp = headers.get(header::CONTENT_SECURITY_POLICY).unwrap();
        let csp_str = csp.to_str().unwrap();
        assert!(csp_str.contains("script-src 'self'"));
        assert!(!csp_str.contains("script-src 'self' 'unsafe-inline'"));
        assert!(csp_str.contains("frame-ancestors 'none'"));
        assert!(csp_str.contains("object-src 'none'"));
    }

    /// WebSocket upgrades from a non-localhost Origin must be rejected. WebSocket
    /// connections do NOT enforce same-origin policy by default, so without this check
    /// any malicious website the user visits could open `ws://localhost:59281/ws` and
    /// stream every session event in real time.
    #[test]
    fn validate_ws_origin_rejects_non_localhost() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://evil.com".parse().unwrap());
        assert!(!validate_ws_origin(&headers));

        headers.insert(header::ORIGIN, "https://localhost:59281".parse().unwrap());
        assert!(
            !validate_ws_origin(&headers),
            "https origin must be rejected, mirrord ui only serves http"
        );
    }

    /// Localhost origins (in any common form) should be accepted.
    #[test]
    fn validate_ws_origin_accepts_localhost() {
        for origin in [
            "http://localhost",
            "http://localhost:59281",
            "http://127.0.0.1:59281",
            "http://[::1]:59281",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert!(
                validate_ws_origin(&headers),
                "should accept origin: {origin}"
            );
        }
    }

    /// Non-browser clients (curl, native code) typically omit the Origin header. We accept
    /// these because the Unix socket and TCP localhost binding are the access control there.
    #[test]
    fn validate_ws_origin_accepts_missing_origin() {
        let headers = HeaderMap::new();
        assert!(validate_ws_origin(&headers));
    }

    /// CSRF check: a malicious form-POST from another origin would not include the cookie
    /// (because of SameSite=Strict), so any state-changing endpoint must reject it.
    #[tokio::test]
    async fn kill_endpoint_without_token_returns_unauthorized() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/sessions/some-id/kill")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::UNAUTHORIZED);
    }

    /// Static assets are served behind the auth middleware so the token cookie is set on
    /// the very first page load (the user clicks `?token=...` in their browser, the static
    /// HTML response sets the cookie, and subsequent API/WS calls authenticate via cookie).
    #[tokio::test]
    async fn static_handler_without_token_returns_unauthorized() {
        assert_eq!(status_of(req("/")).await, StatusCode::UNAUTHORIZED);
    }

    /// The `?token=` query param works for any path, including the root, so the initial
    /// page load establishes the cookie.
    #[tokio::test]
    async fn static_handler_with_token_sets_cookie() {
        let response = build_router(test_state())
            .oneshot(req(&format!("/?token={TEST_TOKEN}")))
            .await
            .unwrap();
        // Status may be 200 (if asset embedded) or 404 (if no embedded assets in test build)
        // either way, the cookie should be set.
        assert!(
            response.headers().get(header::SET_COOKIE).is_some(),
            "cookie must be set on the first authenticated request, even if the body is 404"
        );
    }

    mod operator_sessions {
        use mirrord_operator::crd::session::{
            MirrordSession, MirrordSessionSpec, SessionOwner, SessionTarget,
        };

        use super::*;

        fn sample_cr(name: &str, key: &str) -> MirrordSession {
            let mut cr = MirrordSession::new(
                name,
                MirrordSessionSpec {
                    owner: SessionOwner {
                        user_id: "u".into(),
                        username: "alice".into(),
                        hostname: "h".into(),
                        k8s_username: "alice@ex".into(),
                    },
                    target: Some(SessionTarget {
                        api_version: "apps/v1".into(),
                        kind: "Deployment".into(),
                        name: "web".into(),
                        container: "app".into(),
                    }),
                    key: key.to_owned(),
                    http_filter: None,
                },
            );
            cr.metadata.namespace = Some("default".into());
            cr
        }

        #[test]
        fn summary_from_cr_extracts_key() {
            let cr = sample_cr("cr-1", "alice-session");
            let summary = OperatorSessionSummary::from_cr(&cr).unwrap();
            assert_eq!(summary.name, "cr-1");
            assert_eq!(summary.key.as_deref(), Some("alice-session"));
            assert_eq!(summary.namespace, "default");
            assert_eq!(
                summary.target.as_ref().map(|t| t.name.as_str()),
                Some("web")
            );
        }

        #[tokio::test]
        async fn operator_sessions_groups_by_key_with_none_bucket() {
            let (tx, _rx) = broadcast::channel::<SessionNotification>(16);
            let state = Arc::new(AppState {
                sessions: RwLock::new(HashMap::new()),
                operator_sessions: RwLock::new({
                    let mut m = std::collections::BTreeMap::new();
                    let s1 = OperatorSessionSummary::from_cr(&sample_cr("a", "k")).unwrap();
                    let s2 = OperatorSessionSummary::from_cr(&sample_cr("b", "k")).unwrap();
                    let s3 = OperatorSessionSummary::from_cr(&sample_cr("c", "k2")).unwrap();
                    m.insert("a".into(), s1);
                    m.insert("b".into(), s2);
                    m.insert("c".into(), s3);
                    m
                }),
                operator_watch_status: RwLock::new(OperatorWatchStatus::Watching),
                notify_tx: tx,
                token: "t".into(),
            });

            let resp = list_operator_sessions(axum::extract::State(state)).await.0;
            assert_eq!(resp.sessions.len(), 3);
            assert_eq!(resp.by_key.get("k").map(|v| v.len()), Some(2));
            assert_eq!(resp.by_key.get("k2").map(|v| v.len()), Some(1));
            assert!(matches!(resp.watch_status, OperatorWatchStatus::Watching));
        }
    }
}
