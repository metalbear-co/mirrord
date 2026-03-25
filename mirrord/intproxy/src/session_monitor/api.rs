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
use serde::Serialize;
use tokio::net::UnixListener;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use tokio_util::sync::CancellationToken;

use super::MonitorTx;

#[derive(Clone, Debug, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub process_name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub target: String,
    pub started_at: String,
    pub mirrord_version: String,
    pub is_operator: bool,
    pub processes: Vec<ProcessInfo>,
    pub config: serde_json::Value,
}

struct AppState {
    session_info: SessionInfo,
    monitor_tx: MonitorTx,
    shutdown: CancellationToken,
}

struct SocketCleanup {
    path: PathBuf,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.session_info.clone())
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
    let _ = fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    let _cleanup = SocketCleanup {
        path: socket_path.clone(),
    };

    let state = Arc::new(AppState {
        session_info,
        monitor_tx,
        shutdown: shutdown.clone(),
    });

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
