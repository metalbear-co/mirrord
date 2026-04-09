#[cfg(unix)]
use std::{
    convert::Infallible, fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration,
};

#[cfg(unix)]
use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
#[cfg(unix)]
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use mirrord_session_monitor_protocol::{PortSubscription, ProcessInfo, SessionInfo};
#[cfg(unix)]
use rand::Rng;
#[cfg(unix)]
use serde::Deserialize;
#[cfg(unix)]
use tokio::{
    net::UnixListener,
    sync::{RwLock, broadcast::error::RecvError},
};
#[cfg(unix)]
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
#[cfg(unix)]
use tokio_util::sync::CancellationToken;
#[cfg(unix)]
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    set_header::SetResponseHeaderLayer,
};

#[cfg(unix)]
use super::{MonitorEvent, MonitorTx};

#[cfg(unix)]
struct AppState {
    session_info: RwLock<SessionInfo>,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
    token: String,
}

#[cfg(unix)]
struct SocketCleanup {
    socket_path: PathBuf,
    token_path: PathBuf,
}

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.socket_path) {
            tracing::warn!(?err, path = ?self.socket_path, "Failed to remove session socket");
        }
        if let Err(err) = fs::remove_file(&self.token_path) {
            tracing::warn!(?err, path = ?self.token_path, "Failed to remove session token");
        }
    }
}

#[cfg(unix)]
#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Middleware that validates the request carries a valid auth token, either via the `mirrord_token`
/// cookie or the `?token=` query parameter. On successful query-param auth, a `Set-Cookie` header
/// is added so subsequent requests can use the cookie instead.
#[cfg(unix)]
async fn token_auth(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(query): Query<TokenQuery>,
    request: Request,
    next: Next,
) -> Response {
    // Check cookie first
    if let Some(cookie) = jar.get("mirrord_token")
        && cookie.value() == state.token
    {
        return next.run(request).await;
    }

    // Fall back to query parameter
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
#[cfg(unix)]
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

#[cfg(unix)]
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

#[cfg(unix)]
async fn info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let info = state.session_info.read().await;
    Json(info.clone())
}

#[cfg(unix)]
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
#[cfg(unix)]
async fn kill(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state.shutdown.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

/// Subscribes to monitor events and updates session_info on LayerConnected, LayerDisconnected,
/// and PortSubscription events.
#[cfg(unix)]
async fn update_session_info_from_events(state: Arc<AppState>) {
    let mut rx = match state.monitor_tx.subscribe() {
        Some(rx) => rx,
        None => return,
    };

    loop {
        match rx.recv().await {
            Ok(MonitorEvent::LayerConnected {
                pid,
                parent_pid,
                process_name,
                cmdline,
            }) => {
                let mut info = state.session_info.write().await;
                if !info.processes.iter().any(|p| p.pid == pid) {
                    info.processes.push(ProcessInfo {
                        pid,
                        parent_pid,
                        process_name,
                        cmdline,
                    });
                }
            }
            Ok(MonitorEvent::LayerDisconnected { pid }) => {
                let mut info = state.session_info.write().await;
                info.processes.retain(|p| p.pid != pid);
            }
            Ok(MonitorEvent::PortSubscription { port, mode }) => {
                let mut info = state.session_info.write().await;
                if !info.port_subscriptions.iter().any(|ps| ps.port == port) {
                    info.port_subscriptions
                        .push(PortSubscription { port, mode });
                }
            }
            Ok(_) => {}
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(n, "Session info updater lagged, dropped events");
            }
            Err(RecvError::Closed) => break,
        }
    }
}

#[cfg(unix)]
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
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
        .allow_credentials(true)
}

#[cfg(unix)]
pub async fn start_api_server(
    session_info: SessionInfo,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let session_id = &session_info.session_id;

    // Validate session_id to prevent path traversal
    if session_id.contains('/') || session_id.contains('\\') || session_id.contains("..") {
        return Err(format!("invalid session_id: {session_id}").into());
    }

    let sessions_dir = home::home_dir()
        .ok_or("could not determine home directory")?
        .join(".mirrord")
        .join("sessions");

    fs::create_dir_all(&sessions_dir)?;
    fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700))?;

    // Generate auth token
    let token_bytes: [u8; 32] = rand::rng().random();
    let token = hex::encode(token_bytes);

    // Write token file
    let token_path = sessions_dir.join(format!("{session_id}.token"));
    fs::write(&token_path, &token)?;
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;

    let socket_path = sessions_dir.join(format!("{session_id}.sock"));

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

    // Spawn background task to update session info from monitor events
    tokio::spawn(update_session_info_from_events(state.clone()));

    // Authenticated routes
    let authenticated_routes = Router::new()
        .route("/info", get(info))
        .route("/events", get(events))
        .route("/kill", post(kill))
        .layer(middleware::from_fn_with_state(state.clone(), token_auth));

    let csp_value = HeaderValue::from_static(
        "default-src 'self'; script-src 'none'; object-src 'none'; frame-ancestors 'none'",
    );

    let app = Router::new()
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
        .with_state(state);

    tracing::info!(?socket_path, "Session monitor API server starting");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("Session monitor API server stopped");

    Ok(token)
}
