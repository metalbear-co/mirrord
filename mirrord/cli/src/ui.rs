//! # mirrord UI (Session Monitor)
//!
//! The `mirrord ui` command launches a web-based session monitor that aggregates events from
//! all active mirrord sessions. It watches `~/.mirrord/sessions/` for session sentinel files
//! (`.sock` on unix, `.pipe` on windows), connects to each session's HTTP API over its
//! transport (Unix domain socket or named pipe), and serves a React frontend plus
//! REST/SSE/WebSocket endpoints on localhost.

use std::{
    collections::{BTreeMap, HashMap, hash_map::Entry},
    convert::Infallible,
    fs::File,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
use fs4::fs_std::FileExt;
use futures::stream::StreamExt as _;
use k8s_openapi::{api::authentication::v1::SelfSubjectReview, jiff::Timestamp};
use kube::{Api, Client, api::PostParams};
use mirrord_config::target::{Target, TargetDisplay};
use mirrord_operator::crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME, Session, SessionHttpFilter};
use mirrord_session_monitor_client::{
    SESSION_SENTINEL_EXTENSION, SessionClient, SessionEndpoint, connect_to_session,
    session_endpoints, sessions_dir,
};
use mirrord_session_monitor_protocol::SessionInfo;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rand::Rng;
#[cfg(not(debug_assertions))]
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};
use tracing::{debug, error, info, warn};

use crate::{config::UiArgs, error::CliError, ui::chaos::chaos_router};

mod chaos;

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
        id: String,
    },
    OperatorSessionUpdated {
        session: Box<OperatorSessionSummary>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorSessionSummary {
    pub id: String,
    pub key: String,
    pub namespace: String,
    pub owner: OperatorSessionOwner,
    pub target: Option<OperatorSessionTarget>,
    pub created_at: String,
    #[serde(default)]
    pub duration_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locked_ports: Vec<OperatorLockedPort>,
    #[serde(default)]
    pub queue_splits: OperatorQueueSplits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<SessionHttpFilter>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorLockedPort {
    pub port: u16,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorQueueSplits {
    #[serde(default)]
    pub sqs: usize,
    #[serde(default)]
    pub rabbitmq: usize,
    #[serde(default)]
    pub kafka: usize,
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

impl FromStr for OperatorSessionTarget {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match Target::from_str(s) {
            Ok(Target::Targetless) | Err(_) => Err(()),
            Ok(target) => Ok(OperatorSessionTarget {
                kind: target.type_().to_owned(),
                name: target.name().to_owned(),
                container: String::new(),
            }),
        }
    }
}

impl OperatorSessionSummary {
    fn from_session(session: &Session) -> Option<Self> {
        let id = session.id.clone()?;
        let key = session.key.clone()?;
        let namespace = session.namespace.clone().unwrap_or_default();
        let owner = parse_session_owner(&session.user).unwrap_or_else(|| OperatorSessionOwner {
            username: session.user.clone(),
            k8s_username: session.user.clone(),
        });

        let target = OperatorSessionTarget::from_str(&session.target).ok();

        let created_at = SystemTime::now()
            .checked_sub(Duration::from_secs(session.duration_secs))
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .and_then(|secs| i64::try_from(secs).ok())
            .and_then(|secs| Timestamp::from_second(secs).ok())
            .map(|ts| ts.to_string())?;
        let http_filter = session.http_filter.as_ref().map(|f| SessionHttpFilter {
            header_filter: f.header_filter.clone(),
        });
        let locked_ports = session
            .locked_ports
            .as_ref()
            .map(|ports| {
                ports
                    .iter()
                    .map(|lp| {
                        let lp = lp.to_locked_port();
                        OperatorLockedPort {
                            port: lp.port,
                            kind: lp.kind,
                            filter: lp.filter,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let queue_splits = OperatorQueueSplits {
            sqs: session.sqs.as_ref().map(|v| v.len()).unwrap_or(0),
            rabbitmq: session.rmq.as_ref().map(|v| v.len()).unwrap_or(0),
            kafka: session.kafka.as_ref().map(|v| v.len()).unwrap_or(0),
        };
        Some(Self {
            id,
            key,
            namespace,
            owner,
            target,
            created_at,
            duration_secs: session.duration_secs,
            locked_ports,
            queue_splits,
            http_filter,
        })
    }
}

fn parse_session_owner(user: &str) -> Option<OperatorSessionOwner> {
    let (head, _hostname) = user.rsplit_once('@')?;
    let (username, k8s_username) = head.split_once('/')?;
    Some(OperatorSessionOwner {
        username: username.to_owned(),
        k8s_username: k8s_username.to_owned(),
    })
}

#[derive(Clone, Debug, Serialize)]
struct TrackedSession {
    info: SessionInfo,
    #[serde(skip)]
    endpoint: SessionEndpoint,
    #[serde(skip)]
    events: Vec<serde_json::Value>,
    #[serde(skip)]
    client: SessionClient,
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<RwLock<HashMap<String, TrackedSession>>>,
    operator_sessions: Arc<RwLock<BTreeMap<String, OperatorSessionSummary>>>,
    operator_watch_status: Arc<RwLock<OperatorWatchStatus>>,
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
    State(state): State<AppState>,
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
async fn stream_session_events(session_id: String, client: SessionClient, state: AppState) {
    let mut sse_stream = match client.open_event_stream().await {
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
        && let Err(err) = std::fs::remove_file(&session.endpoint.sentinel_path)
    {
        warn!(%session_id, ?err, "Failed to remove session sentinel");
    }
    remove_session(&session_id, &state).await;
}

async fn scan_existing_sessions(sessions_dir: &std::path::Path, state: &AppState) {
    for (session_id, endpoint) in session_endpoints(sessions_dir) {
        add_session(session_id, endpoint, state.clone()).await;
    }
}

async fn add_session(session_id: String, endpoint: SessionEndpoint, state: AppState) {
    if state.sessions.read().await.contains_key(&session_id) {
        return;
    }

    let connection = match connect_to_session(&endpoint.sentinel_path).await {
        Ok(connection) => connection,
        Err(err) => {
            debug!(%session_id, ?err, "Could not fetch session info, removing stale sentinel");
            if let Err(err) = std::fs::remove_file(&endpoint.sentinel_path) {
                warn!(%session_id, ?err, "Failed to remove stale sentinel");
            }
            return;
        }
    };

    let tracked = TrackedSession {
        endpoint: connection.endpoint.clone(),
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

async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let list: Vec<SessionInfo> = sessions.values().map(|s| s.info.clone()).collect();
    axum::Json(list)
}

async fn get_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let sessions = state.sessions.read().await;
    match sessions.get(&id) {
        Some(session) => axum::Json(session.info.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn session_events_sse(State(state): State<AppState>, Path(id): Path<String>) -> Response {
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

        let mut sse_stream = match client.open_event_stream().await {
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
    by_key: BTreeMap<String, Vec<OperatorSessionSummary>>,
    sessions: Vec<OperatorSessionSummary>,
    watch_status: OperatorWatchStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentUserResponse {
    k8s_username: Option<String>,
    error: Option<String>,
}

async fn current_user() -> axum::Json<CurrentUserResponse> {
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(err) => {
            return axum::Json(CurrentUserResponse {
                k8s_username: None,
                error: Some(format!("kube client init failed: {err}")),
            });
        }
    };
    let api: Api<SelfSubjectReview> = Api::all(client);
    match api
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await
    {
        Ok(review) => {
            let username = review
                .status
                .and_then(|s| s.user_info)
                .and_then(|u| u.username);
            axum::Json(CurrentUserResponse {
                k8s_username: username,
                error: None,
            })
        }
        Err(err) => axum::Json(CurrentUserResponse {
            k8s_username: None,
            error: Some(format!("self-subject-review failed: {err}")),
        }),
    }
}

async fn list_operator_sessions(
    State(state): State<AppState>,
) -> axum::Json<OperatorSessionsResponse> {
    let map = state.operator_sessions.read().await;
    let watch_status = state.operator_watch_status.read().await.clone();

    let mut by_key: BTreeMap<String, Vec<OperatorSessionSummary>> = BTreeMap::new();
    let sessions: Vec<OperatorSessionSummary> = map.values().cloned().collect();
    for s in &sessions {
        by_key.entry(s.key.clone()).or_default().push(s.clone());
    }

    axum::Json(OperatorSessionsResponse {
        by_key,
        sessions,
        watch_status,
    })
}

async fn kill_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let (client, sentinel_path) = {
        let sessions = state.sessions.read().await;
        match sessions.get(&id) {
            Some(session) => (
                session.client.clone(),
                session.endpoint.sentinel_path.clone(),
            ),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    match client.kill().await {
        Ok(()) => {
            // Clean up the sentinel file (the socket on unix; the marker on windows) and
            // remove the session from local tracking. The producer also removes the sentinel
            // on shutdown via its own Drop guard, so this is best-effort.
            if let Err(err) = std::fs::remove_file(&sentinel_path) {
                warn!(%id, ?err, "Failed to remove session sentinel after kill");
            }
            remove_session(&id, &state).await;

            axum::Json(serde_json::json!({"status": "ok"})).into_response()
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
    State(state): State<AppState>,
) -> Response {
    if !validate_ws_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(|socket| ws_connection(socket, state))
}

async fn ws_connection(mut socket: WebSocket, state: AppState) {
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
fn start_periodic_rescan(sessions_dir: PathBuf, state: AppState) {
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
    state: AppState,
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
                if path.extension().and_then(|e| e.to_str()) != Some(SESSION_SENTINEL_EXTENSION) {
                    continue;
                }

                let Some(endpoint) = SessionEndpoint::from_sentinel(path) else {
                    continue;
                };
                let session_id = endpoint.session_id.clone();

                match event.kind {
                    EventKind::Create(_) => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        add_session(session_id, endpoint, state.clone()).await;
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

fn start_operator_watcher(state: AppState) {
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

        let api: Api<MirrordOperatorCrd> = Api::all(client);

        if let Err(err) = api.get(OPERATOR_STATUS_NAME).await {
            let reason = format!("operator not available: {err}");
            warn!("{reason}");
            *state.operator_watch_status.write().await =
                OperatorWatchStatus::Unavailable { reason };
            return;
        }

        *state.operator_watch_status.write().await = OperatorWatchStatus::Watching;

        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match api.get(OPERATOR_STATUS_NAME).await {
                Ok(operator) => reconcile_operator_sessions(&state, &operator).await,
                Err(err) => {
                    let reason = format!("operator status fetch error: {err}");
                    warn!("{reason}");
                    *state.operator_watch_status.write().await =
                        OperatorWatchStatus::Error { message: reason };
                }
            }
        }
    });
}

async fn reconcile_operator_sessions(state: &AppState, operator: &MirrordOperatorCrd) {
    let observed: HashMap<String, OperatorSessionSummary> = operator
        .status
        .as_ref()
        .map(|s| s.sessions.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(OperatorSessionSummary::from_session)
        .map(|s| (s.id.clone(), s))
        .collect();

    let mut map = state.operator_sessions.write().await;
    apply_observed_sessions(&mut map, &observed, &state.notify_tx);
    drop_stale_sessions(&mut map, &observed, &state.notify_tx);
}

fn apply_observed_sessions(
    map: &mut BTreeMap<String, OperatorSessionSummary>,
    observed: &HashMap<String, OperatorSessionSummary>,
    notify_tx: &broadcast::Sender<SessionNotification>,
) {
    for (id, summary) in observed {
        let prior = map.insert(id.clone(), summary.clone());
        let event = if prior.is_none() {
            SessionNotification::OperatorSessionAdded {
                session: Box::new(summary.clone()),
            }
        } else {
            SessionNotification::OperatorSessionUpdated {
                session: Box::new(summary.clone()),
            }
        };
        let _ = notify_tx.send(event);
    }
}

fn drop_stale_sessions(
    map: &mut BTreeMap<String, OperatorSessionSummary>,
    observed: &HashMap<String, OperatorSessionSummary>,
    notify_tx: &broadcast::Sender<SessionNotification>,
) {
    let stale: Vec<String> = map
        .keys()
        .filter(|id| !observed.contains_key(*id))
        .cloned()
        .collect();
    for id in stale {
        map.remove(&id);
        let _ = notify_tx.send(SessionNotification::OperatorSessionRemoved { id });
    }
}

async fn health() -> impl IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

fn build_router(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/events", get(session_events_sse))
        .route("/sessions/{id}/kill", post(kill_session))
        .route("/operator-sessions", get(list_operator_sessions))
        .route("/me", get(current_user));

    let authenticated_routes = Router::new()
        .nest("/chaos/rules", chaos_router(state.clone()))
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
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Returns the path to the UI token file, `~/.mirrord/token`.
///
/// The running `mirrord ui` writes its auth token here so a second invocation can read it back and
/// print a working URL. This file is never locked — on Windows an exclusive lock is mandatory and
/// would stop the second process from reading it — so mutual exclusion lives in a separate lock
/// file (see [`lock_file_path`]).
fn token_file_path() -> Option<PathBuf> {
    home::home_dir().map(|home| home.join(".mirrord").join("token"))
}

/// Returns the path to the UI lock file, `~/.mirrord/ui.lock`.
///
/// A running `mirrord ui` holds an exclusive lock on this file. The lock — released by the OS even
/// on a hard kill — is how a second invocation detects that one is already running. It is kept
/// separate from the token file so the token stays freely readable on all platforms.
fn lock_file_path() -> Option<PathBuf> {
    home::home_dir().map(|home| home.join(".mirrord").join("ui.lock"))
}

/// Keeps the lock file open and exclusively locked for as long as the UI server runs.
///
/// Dropping the guard removes the token file, signalling that the UI is no longer running. A hard
/// kill skips this [`Drop`] and leaves the token file behind, but the OS releases the lock on
/// process exit, so the next invocation still sees the previous instance is gone, reclaims the
/// lock, and overwrites the stale token.
struct TokenFileGuard {
    /// Held open to keep the exclusive lock; the lock file itself is left in place (removing it
    /// while the handle is open is racy and a leftover empty file is harmless — the next run
    /// re-locks it). The lock releases when this handle closes, including on a hard kill.
    lock_file: File,
    token_path: PathBuf,
}

impl Drop for TokenFileGuard {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.token_path) {
            warn!(?err, path = %self.token_path.display(), "Failed to remove UI token file");
        }
        let _ = FileExt::unlock(&self.lock_file);
    }
}

/// Result of trying to claim ownership of the UI lock.
enum TokenClaim {
    /// No other `mirrord ui` was running; we now hold the lock and published `token`.
    Claimed {
        guard: TokenFileGuard,
        token: String,
    },
    /// Another `mirrord ui` is already running; `token` is the one it published.
    AlreadyRunning { token: String },
}

/// Tries to become the single running `mirrord ui` instance by taking an exclusive lock on the
/// lock file. If another instance already holds it, reads back the token it published.
fn claim_token_file() -> Result<TokenClaim, CliError> {
    let lock_path = lock_file_path()
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?;
    let token_path = token_file_path()
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?;
    claim_token_file_at(lock_path, token_path)
}

fn claim_token_file_at(lock_path: PathBuf, token_path: PathBuf) -> Result<TokenClaim, CliError> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::UiError(format!("failed to create mirrord directory: {e}")))?;
    }

    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| CliError::UiError(format!("failed to open lock file: {e}")))?;

    if !lock_file
        .try_lock_exclusive()
        .map_err(|e| CliError::UiError(format!("failed to lock lock file: {e}")))?
    {
        let token = std::fs::read_to_string(&token_path)
            .map_err(|e| CliError::UiError(format!("failed to read token file: {e}")))?;
        return Ok(TokenClaim::AlreadyRunning {
            token: token.trim().to_owned(),
        });
    }

    let token_bytes: [u8; 32] = rand::rng().random();
    let token = hex::encode(token_bytes);
    std::fs::write(&token_path, &token)
        .map_err(|e| CliError::UiError(format!("failed to write token file: {e}")))?;

    Ok(TokenClaim::Claimed {
        guard: TokenFileGuard {
            lock_file,
            token_path,
        },
        token,
    })
}

/// Outcome of [`setup_ui`].
pub enum UiSetup {
    /// This invocation owns the UI; `server` runs it and `url` opens it.
    Started {
        server: futures::future::BoxFuture<'static, Result<(), CliError>>,
        url: String,
    },
    /// Another `mirrord ui` is already running; `url` opens the existing instance.
    AlreadyRunning { url: String },
}

pub async fn ui_command(args: UiArgs) -> Result<(), CliError> {
    match setup_ui(args.port).await? {
        UiSetup::AlreadyRunning { url } => {
            eprintln!();
            eprintln!("  mirrord session monitor is already running");
            eprintln!("    Web UI:             {url}");
            eprintln!();
            Ok(())
        }
        UiSetup::Started { server, url } => {
            if !args.no_browser {
                if let Err(err) = opener::open_browser(&url) {
                    warn!(?err, "Failed to open browser");
                }
            }

            eprintln!();
            eprintln!("  mirrord session monitor");
            eprintln!("    Web UI:             {url}");
            eprintln!();

            server.await
        }
    }
}

pub async fn setup_ui(port: u16) -> Result<UiSetup, CliError> {
    let (guard, token) = match claim_token_file()? {
        TokenClaim::AlreadyRunning { token } => {
            let url = format!("http://{}:{port}?token={token}", Ipv4Addr::LOCALHOST);
            return Ok(UiSetup::AlreadyRunning { url });
        }
        TokenClaim::Claimed { guard, token } => (guard, token),
    };

    let sessions_dir = sessions_dir()
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?;

    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| CliError::UiError(format!("failed to create sessions directory: {e}")))?;

    let (notify_tx, _) = broadcast::channel::<SessionNotification>(256);

    let state = AppState {
        sessions: Default::default(),
        operator_sessions: Default::default(),
        operator_watch_status: Default::default(),
        notify_tx,
        token: token.clone(),
    };

    scan_existing_sessions(&sessions_dir, &state).await;
    #[cfg(target_os = "macos")]
    start_periodic_rescan(sessions_dir.clone(), state.clone());
    start_filesystem_watcher(&sessions_dir, state.clone())?;
    start_operator_watcher(state.clone());

    let app = build_router(state);

    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| CliError::UiError(format!("failed to bind to {addr}: {e}")))?;

    let addr = listener
        .local_addr()
        .map_err(|e| CliError::UiError(format!("failed to get listener address: {e}")))?;
    let url = format!("http://{addr}?token={token}");

    let server = Box::pin(async move {
        // Held until the server stops so the token file is removed on graceful shutdown.
        let _guard = guard;
        axum::serve(listener, app)
            .await
            .map_err(|e| CliError::UiError(format!("server error: {e}")))
    });

    Ok(UiSetup::Started { server, url })
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

    fn test_state() -> AppState {
        let (notify_tx, _) = broadcast::channel(16);
        AppState {
            sessions: Default::default(),
            operator_sessions: Default::default(),
            operator_watch_status: Default::default(),
            notify_tx,
            token: TEST_TOKEN.to_owned(),
        }
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

    mod token_file {
        use super::*;

        fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
            (dir.path().join("ui.lock"), dir.path().join("token"))
        }

        /// The first claim takes the lock and writes a fresh token to the token file.
        #[test]
        fn first_claim_succeeds_and_writes_token() {
            let dir = tempfile::tempdir().unwrap();
            let (lock, token_path) = paths(&dir);

            let TokenClaim::Claimed { guard, token } =
                claim_token_file_at(lock, token_path.clone()).unwrap()
            else {
                panic!("first claim should succeed");
            };

            assert!(!token.is_empty());
            assert_eq!(std::fs::read_to_string(&token_path).unwrap(), token);
            drop(guard);
        }

        /// While one claim holds the lock, a second claim reports the UI is already running and
        /// hands back the same token, so the caller can build a working URL.
        #[test]
        fn second_claim_while_held_returns_already_running_with_same_token() {
            let dir = tempfile::tempdir().unwrap();
            let (lock, token_path) = paths(&dir);

            let TokenClaim::Claimed { guard, token } =
                claim_token_file_at(lock.clone(), token_path.clone()).unwrap()
            else {
                panic!("first claim should succeed");
            };

            match claim_token_file_at(lock, token_path).unwrap() {
                TokenClaim::AlreadyRunning { token: seen } => assert_eq!(seen, token),
                TokenClaim::Claimed { .. } => panic!("second claim should see the lock held"),
            }

            drop(guard);
        }

        /// Dropping the guard removes the token file and releases the lock, so a later invocation
        /// claims it fresh — this is how a clean shutdown signals the UI is gone.
        #[test]
        fn dropping_guard_removes_file_and_allows_reclaim() {
            let dir = tempfile::tempdir().unwrap();
            let (lock, token_path) = paths(&dir);

            let TokenClaim::Claimed {
                guard,
                token: first,
            } = claim_token_file_at(lock.clone(), token_path.clone()).unwrap()
            else {
                panic!("first claim should succeed");
            };
            drop(guard);
            assert!(
                !token_path.exists(),
                "guard drop should remove the token file"
            );

            let TokenClaim::Claimed { token: second, .. } =
                claim_token_file_at(lock, token_path).unwrap()
            else {
                panic!("reclaim after drop should succeed");
            };
            assert_ne!(first, second, "a reclaim should publish a fresh token");
        }
    }

    mod operator_sessions {
        use mirrord_operator::crd::Session;

        use super::*;

        fn sample_session(id: &str, key: &str) -> Session {
            Session {
                id: Some(id.to_owned()),
                duration_secs: 60,
                user: "alice/alice@ex@host".to_owned(),
                target: "deployment/web".to_owned(),
                namespace: Some("default".to_owned()),
                locked_ports: None,
                user_id: Some("u".to_owned()),
                sqs: None,
                rmq: None,
                kafka: None,
                key: Some(key.to_owned()),
                http_filter: None,
            }
        }

        #[test]
        fn summary_from_session_extracts_key_and_parses_target_owner() {
            let session = sample_session("cr-1", "alice-session");
            let summary = OperatorSessionSummary::from_session(&session).unwrap();
            assert_eq!(summary.id, "cr-1");
            assert_eq!(summary.key, "alice-session");
            assert_eq!(summary.namespace, "default");
            assert_eq!(
                summary.target.as_ref().map(|t| t.name.as_str()),
                Some("web")
            );
            assert_eq!(
                summary.target.as_ref().map(|t| t.kind.as_str()),
                Some("deployment")
            );
            assert_eq!(summary.owner.username, "alice");
            assert_eq!(summary.owner.k8s_username, "alice@ex");
        }

        #[tokio::test]
        async fn operator_sessions_groups_by_key_with_none_bucket() {
            let (tx, _rx) = broadcast::channel::<SessionNotification>(16);
            let state = AppState {
                sessions: Default::default(),
                operator_sessions: Arc::new(RwLock::new({
                    let mut m = BTreeMap::new();
                    let s1 =
                        OperatorSessionSummary::from_session(&sample_session("a", "k")).unwrap();
                    let s2 =
                        OperatorSessionSummary::from_session(&sample_session("b", "k")).unwrap();
                    let s3 =
                        OperatorSessionSummary::from_session(&sample_session("c", "k2")).unwrap();
                    m.insert("a".into(), s1);
                    m.insert("b".into(), s2);
                    m.insert("c".into(), s3);
                    m
                })),
                operator_watch_status: Arc::new(RwLock::new(OperatorWatchStatus::Watching)),
                notify_tx: tx,
                token: "t".into(),
            };

            let resp = list_operator_sessions(axum::extract::State(state)).await.0;
            assert_eq!(resp.sessions.len(), 3);
            assert_eq!(resp.by_key.get("k").map(|v| v.len()), Some(2));
            assert_eq!(resp.by_key.get("k2").map(|v| v.len()), Some(1));
            assert!(matches!(resp.watch_status, OperatorWatchStatus::Watching));
        }
    }
}
