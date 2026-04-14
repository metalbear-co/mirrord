#[cfg(unix)]
use std::{
    convert::Infallible,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
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
use super::{MonitorEvent, MonitorTx};

/// Per-session API state. Access control is provided by Unix socket file
/// permissions (`0o600`), so the HTTP layer itself is unauthenticated.
#[cfg(unix)]
struct AppState {
    session_info: RwLock<SessionInfo>,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
}

#[cfg(unix)]
struct SocketCleanup {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::warn!(?err, path = ?self.path, "Failed to remove session socket");
        }
    }
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

/// Tears down the mirrord session: `SIGTERM`s every layer-connected process so the user
/// application exits, then cancels the API server's shutdown token. Once the user app
/// processes die, their layer connections close and the intproxy exits on its idle timeout.
///
/// Without this, `mirrord session delete` (or the UI Kill button) only removes the socket
/// while the user app and intproxy keep running, leaving zombie steal/mirror subscriptions
/// on the cluster pod.
#[cfg(unix)]
async fn kill(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pids: Vec<u32> = {
        let info = state.session_info.read().await;
        info.processes.iter().map(|p| p.pid).collect()
    };

    for pid in &pids {
        // SAFETY: `kill(2)` with a process-group or PID argument is a single syscall that
        // cannot corrupt our process. If the PID is gone or we lack permission, kill returns
        // -1 and sets errno; we log and continue.
        let ret = unsafe { libc::kill(*pid as libc::pid_t, libc::SIGTERM) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(pid = *pid, %err, "Failed to SIGTERM layer-connected process");
        }
    }

    state.shutdown.cancel();
    Json(serde_json::json!({"status": "shutting_down", "signaled_pids": pids}))
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
#[cfg(unix)]
fn verify_session_id(session_id: &str) -> bool {
    let as_path = Path::new(session_id);
    let mut components = as_path.components();
    matches!(
        std::array::from_fn::<_, 2, _>(|_| components.next()),
        [Some(Component::Normal(..)), None]
    )
}

#[cfg(unix)]
pub async fn start_api_server(
    session_info: SessionInfo,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_id = &session_info.session_id;

    if !verify_session_id(session_id) {
        return Err(format!("invalid session_id: {session_id}").into());
    }

    let sessions_dir = home::home_dir()
        .ok_or("could not determine home directory")?
        .join(".mirrord")
        .join("sessions");

    fs::create_dir_all(&sessions_dir)?;
    fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700))?;

    let socket_path = sessions_dir.join(format!("{session_id}.sock"));

    if let Err(err) = fs::remove_file(&socket_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(?err, ?socket_path, "Failed to remove stale session socket");
    }

    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    let _cleanup = SocketCleanup {
        path: socket_path.clone(),
    };

    let state = Arc::new(AppState {
        session_info: RwLock::new(session_info),
        monitor_tx,
        shutdown: shutdown.clone(),
    });

    tokio::spawn(update_session_info_from_events(state.clone()));

    let app = Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/events", get(events))
        .route("/kill", post(kill))
        .with_state(state);

    tracing::info!(?socket_path, "Session monitor API server starting");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("Session monitor API server stopped");

    Ok(())
}
