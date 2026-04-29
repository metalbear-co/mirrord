//! Per-session HTTP API. Routes (`/health`, `/info`, `/events`, `/kill`) are platform-agnostic
//! axum handlers; the only thing that varies between unix and windows is the transport the
//! API binds to:
//!
//! - **unix**: `UnixListener` at `{sessions_dir}/{session_id}.sock`, mode `0o600`.
//! - **windows**: `NamedPipeServer` at `\\.\pipe\mirrord-session-{session_id}` with a DACL
//!   restricted to the current user. A zero-byte sentinel file at
//!   `{sessions_dir}/{session_id}.pipe` lets file-watcher-based discovery work the same way the
//!   unix socket does.
//!
//! Confidentiality and authentication come from the OS-level access control on the transport
//! itself; the HTTP layer is unauthenticated.

use std::{
    convert::Infallible,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use mirrord_session_monitor_protocol::{PortSubscription, ProcessInfo, SessionInfo};
use tokio::sync::{RwLock, broadcast::error::RecvError};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use tokio_util::sync::CancellationToken;

use super::{MonitorEvent, MonitorTx};

#[cfg(windows)]
#[path = "transport_windows.rs"]
mod transport_windows;
#[cfg(windows)]
#[path = "win_security.rs"]
mod win_security;

/// Per-session API state. Access control is provided by the OS-level permissions on the
/// transport (`0o600` on the unix socket; restrictive DACL on the named pipe), so the HTTP
/// layer itself is unauthenticated.
struct AppState {
    session_info: RwLock<SessionInfo>,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
}

/// Drops the transport's filesystem footprint when `start_api_server` returns. On unix this
/// is the socket itself; on windows it is the sentinel marker file (the named pipe has no
/// persistent file).
struct TransportCleanup {
    path: PathBuf,
}

impl Drop for TransportCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::warn!(?err, path = ?self.path, "Failed to remove session sentinel");
        }
    }
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

async fn update_session_info_from_events(
    state: Arc<AppState>,
    mut rx: tokio::sync::broadcast::Receiver<MonitorEvent>,
) {
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
                match info
                    .port_subscriptions
                    .iter_mut()
                    .find(|ps| ps.port == port)
                {
                    Some(existing) => existing.mode = mode,
                    None => info
                        .port_subscriptions
                        .push(PortSubscription { port, mode }),
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

/// Returns `true` when `session_id` is a single normal path component, so that joining it with
/// the sessions directory cannot escape that directory or produce an absolute path.
fn verify_session_id(session_id: &str) -> bool {
    let as_path = Path::new(session_id);
    let mut components = as_path.components();
    matches!(
        std::array::from_fn::<_, 2, _>(|_| components.next()),
        [Some(Component::Normal(..)), None]
    )
}

/// Starts the per-session API server.
///
/// `sessions_dir` is created on demand (and on unix, set to mode `0o700`). The transport binds
/// at `{sessions_dir}/{session_id}.sock` (unix) or `\\.\pipe\mirrord-session-{session_id}`
/// with a sentinel file at `{sessions_dir}/{session_id}.pipe` (windows). Tests pass a tempdir;
/// the production caller passes `~/.mirrord/sessions`.
pub async fn start_api_server(
    sessions_dir: PathBuf,
    session_info: SessionInfo,
    monitor_tx: MonitorTx,
    monitor_rx: tokio::sync::broadcast::Receiver<MonitorEvent>,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_id = session_info.session_id.clone();

    if !verify_session_id(&session_id) {
        return Err(format!("invalid session_id: {session_id}").into());
    }

    fs::create_dir_all(&sessions_dir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700))?;
    }

    let state = Arc::new(AppState {
        session_info: RwLock::new(session_info),
        monitor_tx,
        shutdown: shutdown.clone(),
    });

    tokio::spawn(update_session_info_from_events(state.clone(), monitor_rx));

    let app = Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/events", get(events))
        .route("/kill", post(kill))
        .with_state(state);

    serve_session(&sessions_dir, &session_id, app, shutdown).await
}

#[cfg(unix)]
async fn serve_session(
    sessions_dir: &Path,
    session_id: &str,
    app: Router,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::os::unix::fs::PermissionsExt;

    use tokio::net::UnixListener;

    let socket_path = sessions_dir.join(format!("{session_id}.sock"));

    if let Err(err) = fs::remove_file(&socket_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(?err, ?socket_path, "Failed to remove stale session socket");
    }

    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    let _cleanup = TransportCleanup {
        path: socket_path.clone(),
    };

    tracing::info!(?socket_path, "Session monitor API server starting");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("Session monitor API server stopped");

    Ok(())
}

#[cfg(windows)]
async fn serve_session(
    sessions_dir: &Path,
    session_id: &str,
    app: Router,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use mirrord_session_monitor_protocol::pipe_name_for_session;
    use transport_windows::NamedPipeListener;

    let sentinel_path = sessions_dir.join(format!("{session_id}.pipe"));
    let pipe_name = pipe_name_for_session(session_id);

    // Sentinel file lets the consumer's file-watcher discover the session the same way it
    // does on unix. Content is intentionally empty.
    fs::write(&sentinel_path, b"")?;

    let _cleanup = TransportCleanup {
        path: sentinel_path.clone(),
    };

    let listener = NamedPipeListener::bind(pipe_name.clone())?;

    tracing::info!(?sentinel_path, %pipe_name, "Session monitor API server starting");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("Session monitor API server stopped");

    Ok(())
}
