use std::{
    convert::Infallible, fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration,
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
    #[serde(default)]
    pub parent_pid: Option<u32>,
    pub process_name: String,
    #[serde(default)]
    pub cmdline: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    #[serde(default)]
    pub key: Option<String>,
    pub target: String,
    #[serde(default)]
    pub namespace: Option<String>,
    pub started_at: String,
    pub mirrord_version: String,
    pub is_operator: bool,
    #[serde(default)]
    pub processes: Vec<ProcessInfo>,
    pub config: serde_json::Value,
}

struct AppState {
    session_info: RwLock<SessionInfo>,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
}

struct SocketCleanup {
    path: PathBuf,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::warn!(?err, path = ?self.path, "Failed to remove session socket");
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

/// Subscribes to monitor events and updates session_info.processes on
/// LayerConnected/LayerDisconnected events.
async fn update_processes_from_events(state: Arc<AppState>) {
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_id = &session_info.session_id;
    let sessions_dir = home::home_dir()
        .ok_or("could not determine home directory")?
        .join(".mirrord")
        .join("sessions");

    fs::create_dir_all(&sessions_dir)?;
    fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700))?;

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
        path: socket_path.clone(),
    };

    let state = Arc::new(AppState {
        session_info: RwLock::new(session_info),
        monitor_tx,
        shutdown: shutdown.clone(),
    });

    // Spawn background task to update processes from monitor events
    tokio::spawn(update_processes_from_events(state.clone()));

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

#[cfg(test)]
mod tests {
    use super::SessionInfo;

    #[test]
    fn deserialize_session_info_from_older_payload() {
        let json = serde_json::json!({
            "session_id": "session-id",
            "target": "pod/test",
            "started_at": "2026-04-07T20:33:29Z",
            "mirrord_version": "3.199.0",
            "is_operator": false,
            "processes": [
                {
                    "pid": 1234,
                    "process_name": "bash"
                }
            ],
            "config": {}
        });

        let session: SessionInfo = serde_json::from_value(json).unwrap();

        assert_eq!(session.key, None);
        assert_eq!(session.namespace, None);
        assert_eq!(session.processes.len(), 1);
        assert_eq!(session.processes[0].parent_pid, None);
        assert_eq!(session.processes[0].cmdline, Vec::<String>::new());
    }
}
