//! Locks the wire contract between [`mirrord_intproxy::session_monitor::api`] (the per-session
//! HTTP server) and [`mirrord_session_monitor_client`] (the consumer used by `mirrord ui`).
//!
//! The transport is currently Unix domain sockets, hence the file-level [`cfg(unix)`]. Any
//! future Windows port of this stack must keep these tests passing on both platforms with the
//! same assertions. The cfg widens to the platforms the port supports; the assertions don't
//! change.
//!
//! What we lock down per-test:
//! - `/info` returns the [`SessionInfo`] passed at startup.
//! - `/health` returns `{"status":"ok"}`.
//! - `/events` is an SSE endpoint serving JSON-encoded [`MonitorEvent`]s with a stable shape.
//! - The internal updater task mutates `processes` on `LayerConnected` / `LayerDisconnected` and
//!   de-duplicates by pid.
//! - The internal updater task upserts `port_subscriptions` on `PortSubscription` (no duplicates
//!   for the same port; the mode replaces in place).
//! - `start_api_server` rejects session ids that aren't a single normal path component.
//! - The sessions directory is `0o700` and the per-session socket is `0o600`.
//! - `/kill` triggers graceful shutdown and the socket file is removed on the way out.

#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use eventsource_stream::Eventsource;
use futures::StreamExt;
use mirrord_intproxy::session_monitor::{MonitorEvent, MonitorTx, api::start_api_server};
use mirrord_session_monitor_client::{
    SessionConnection, connect_to_session, fetch_session_info, kill_session,
};
use mirrord_session_monitor_protocol::SessionInfo;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

fn synthetic_session_info(id: &str) -> SessionInfo {
    SessionInfo {
        session_id: id.to_owned(),
        key: Some("test-key".to_owned()),
        target: "deployment/web".to_owned(),
        namespace: Some("default".to_owned()),
        started_at: "2020-01-01T00:00:00Z".to_owned(),
        mirrord_version: "0.0.0-test".to_owned(),
        is_operator: false,
        processes: Vec::new(),
        port_subscriptions: Vec::new(),
        config: serde_json::json!({}),
    }
}

async fn wait_for_socket(path: &Path, deadline: Duration) {
    let start = tokio::time::Instant::now();
    while !path.exists() {
        if start.elapsed() >= deadline {
            panic!("server did not bind socket at {path:?} within {deadline:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn poll_until<T, Fut, F>(deadline: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = tokio::time::Instant::now();
    loop {
        if let Some(value) = f().await {
            return Some(value);
        }
        if start.elapsed() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Spins up the API server with a synthetic [`SessionInfo`] in a tempdir-backed sessions
/// directory. Tests connect via the client crate, run their assertions, and call
/// [`Self::shutdown_via_kill`] at the end so the server exits cleanly and the socket gets
/// removed by [`SocketCleanup`]'s `Drop`.
struct StartedServer {
    _sessions_tempdir: TempDir,
    socket_path: PathBuf,
    monitor_tx: MonitorTx,
    server: tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
}

impl StartedServer {
    async fn start(session_id: &str) -> Self {
        Self::start_with_info(session_id, synthetic_session_info(session_id)).await
    }

    async fn start_with_info(session_id: &str, info: SessionInfo) -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let sessions_dir = tempdir.path().to_path_buf();
        let socket_path = sessions_dir.join(format!("{session_id}.sock"));
        let (tx, rx) = broadcast::channel::<MonitorEvent>(64);
        let monitor_tx = MonitorTx::from_sender(tx);
        let shutdown = CancellationToken::new();

        let server = tokio::spawn({
            let monitor_tx = monitor_tx.clone();
            let sessions_dir = sessions_dir.clone();
            async move { start_api_server(sessions_dir, info, monitor_tx, rx, shutdown).await }
        });
        wait_for_socket(&socket_path, Duration::from_secs(5)).await;

        Self {
            _sessions_tempdir: tempdir,
            socket_path,
            monitor_tx,
            server,
        }
    }

    /// Sends `/kill` over `conn`, drops every consumer-side reference (so axum's graceful
    /// shutdown can drain), and awaits the server task. Asserts the socket file was cleaned up.
    async fn shutdown_via_kill(self, conn: SessionConnection) {
        let client = conn.client.clone();
        kill_session(&client).await.expect("kill request");
        drop(client);
        drop(conn);
        tokio::time::timeout(Duration::from_secs(10), self.server)
            .await
            .expect("server did not shut down within 10s")
            .expect("server task panicked")
            .expect("server returned error");
        assert!(
            !self.socket_path.exists(),
            "socket file should be removed on shutdown"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_round_trips_session_passed_at_startup() {
    let server = StartedServer::start("info-roundtrip").await;
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");

    assert_eq!(conn.info.session_id, "info-roundtrip");
    assert_eq!(conn.info.target, "deployment/web");
    assert_eq!(conn.info.namespace.as_deref(), Some("default"));
    assert_eq!(conn.info.key.as_deref(), Some("test-key"));
    assert!(!conn.info.is_operator);
    assert!(conn.info.processes.is_empty());
    assert!(conn.info.port_subscriptions.is_empty());

    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_endpoint_returns_status_ok() {
    let server = StartedServer::start("health").await;
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");

    let resp = conn
        .client
        .get("http://localhost/health")
        .send()
        .await
        .expect("health request");
    assert!(
        resp.status().is_success(),
        "/health returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("health body json");
    assert_eq!(body, serde_json::json!({"status": "ok"}));

    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_serialize_layer_connected_with_expected_shape() {
    let server = StartedServer::start("events-shape").await;
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");
    let client = conn.client.clone();

    let resp = client
        .get("http://localhost/events")
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("events request");
    assert!(resp.status().is_success());
    let mut sse_stream = resp.bytes_stream().eventsource();

    server.monitor_tx.emit(MonitorEvent::LayerConnected {
        pid: 4242,
        parent_pid: Some(1),
        process_name: "test-proc".to_owned(),
        cmdline: vec!["test-proc".to_owned(), "--flag".to_owned()],
    });

    let event = tokio::time::timeout(Duration::from_secs(3), sse_stream.next())
        .await
        .expect("SSE event timeout")
        .expect("SSE stream closed unexpectedly")
        .expect("SSE parse error");
    let parsed: serde_json::Value =
        serde_json::from_str(&event.data).expect("event data must be JSON");
    assert_eq!(
        parsed,
        serde_json::json!({
            "type": "layer_connected",
            "pid": 4242,
            "parent_pid": 1,
            "process_name": "test-proc",
            "cmdline": ["test-proc", "--flag"],
        })
    );

    drop(sse_stream);
    drop(client);
    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_reflects_layer_connected_then_disconnected() {
    let server = StartedServer::start("layer-lifecycle").await;
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");
    let client = conn.client.clone();

    server.monitor_tx.emit(MonitorEvent::LayerConnected {
        pid: 4242,
        parent_pid: Some(1),
        process_name: "test-proc".to_owned(),
        cmdline: vec!["test-proc".to_owned()],
    });

    let info = poll_until(Duration::from_secs(2), || async {
        fetch_session_info(&client)
            .await
            .ok()
            .filter(|info| !info.processes.is_empty())
    })
    .await
    .expect("processes should be populated after LayerConnected");
    let process = info
        .processes
        .first()
        .expect("processes should have one entry");
    assert_eq!(info.processes.len(), 1);
    assert_eq!(process.pid, 4242);
    assert_eq!(process.parent_pid, Some(1));
    assert_eq!(process.process_name, "test-proc");
    assert_eq!(process.cmdline, vec!["test-proc"]);

    server
        .monitor_tx
        .emit(MonitorEvent::LayerDisconnected { pid: 4242 });

    let info = poll_until(Duration::from_secs(2), || async {
        fetch_session_info(&client)
            .await
            .ok()
            .filter(|info| info.processes.is_empty())
    })
    .await
    .expect("processes should be cleared after LayerDisconnected");
    assert!(info.processes.is_empty());

    drop(client);
    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_layer_connected_pid_is_not_duplicated_in_info() {
    let server = StartedServer::start("layer-dedup").await;
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");
    let client = conn.client.clone();

    let make_event = || MonitorEvent::LayerConnected {
        pid: 4242,
        parent_pid: None,
        process_name: "first".to_owned(),
        cmdline: Vec::new(),
    };
    server.monitor_tx.emit(make_event());
    server.monitor_tx.emit(make_event());

    // Wait for the first event to land, then give the updater time to (not) add a second
    // entry for the same pid.
    let _ = poll_until(Duration::from_secs(2), || async {
        fetch_session_info(&client)
            .await
            .ok()
            .filter(|info| !info.processes.is_empty())
    })
    .await
    .expect("processes should be populated after LayerConnected");

    tokio::time::sleep(Duration::from_millis(200)).await;
    let info = fetch_session_info(&client)
        .await
        .expect("info after duplicate emit");
    assert_eq!(
        info.processes.len(),
        1,
        "duplicate pid must not produce a second processes entry"
    );

    drop(client);
    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_reflects_port_subscription_upsert_by_port() {
    let server = StartedServer::start("port-upsert").await;
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");
    let client = conn.client.clone();

    server.monitor_tx.emit(MonitorEvent::PortSubscription {
        port: 8080,
        mode: "mirror".to_owned(),
    });

    let info = poll_until(Duration::from_secs(2), || async {
        fetch_session_info(&client)
            .await
            .ok()
            .filter(|info| !info.port_subscriptions.is_empty())
    })
    .await
    .expect("port subscriptions should be populated");
    let port_sub = info
        .port_subscriptions
        .first()
        .expect("port subscription entry");
    assert_eq!(port_sub.port, 8080);
    assert_eq!(port_sub.mode, "mirror");

    // Re-emit for the same port with a new mode → updater must replace in place rather than
    // pushing a second entry.
    server.monitor_tx.emit(MonitorEvent::PortSubscription {
        port: 8080,
        mode: "steal".to_owned(),
    });

    let info = poll_until(Duration::from_secs(2), || async {
        fetch_session_info(&client).await.ok().filter(|info| {
            info.port_subscriptions
                .first()
                .map(|p| p.mode == "steal")
                .unwrap_or(false)
        })
    })
    .await
    .expect("port subscription mode should be updated to steal");
    assert_eq!(
        info.port_subscriptions.len(),
        1,
        "upsert must not append a second entry for the same port"
    );

    drop(client);
    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_session_id_with_path_traversal_returns_error() {
    let tempdir = TempDir::new().expect("tempdir");
    let (tx, rx) = broadcast::channel::<MonitorEvent>(64);
    let monitor_tx = MonitorTx::from_sender(tx);
    let shutdown = CancellationToken::new();

    let info = synthetic_session_info("../escape");
    let result =
        start_api_server(tempdir.path().to_path_buf(), info, monitor_tx, rx, shutdown).await;

    let err = result.expect_err("session_id with path traversal must be rejected");
    assert!(
        err.to_string().contains("invalid session_id"),
        "error message should mention invalid session_id, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_dir_is_0o700_and_socket_is_0o600() {
    let server = StartedServer::start("perms").await;

    let sessions_dir = server
        .socket_path
        .parent()
        .expect("socket has a parent dir");
    let dir_mode = std::fs::metadata(sessions_dir)
        .expect("sessions dir metadata")
        .permissions()
        .mode()
        & 0o777;
    let socket_mode = std::fs::metadata(&server.socket_path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(dir_mode, 0o700, "sessions directory must be 0o700");
    assert_eq!(socket_mode, 0o600, "session socket must be 0o600");

    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");
    server.shutdown_via_kill(conn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_terminates_server_and_removes_socket() {
    let server = StartedServer::start("kill-cleanup").await;
    let socket_path = server.socket_path.clone();
    let conn = connect_to_session(&server.socket_path)
        .await
        .expect("connect");

    // shutdown_via_kill itself asserts the socket is gone, but capture the path before it's
    // consumed so the assertion below pins the contract from the test's POV too.
    server.shutdown_via_kill(conn).await;
    assert!(!socket_path.exists());
}
