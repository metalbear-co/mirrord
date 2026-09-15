//! Daemon-owned DB branch port forwards shared by local mirrord sessions.
//!
//! An intproxy resolves each configured DB branch endpoint and sends an authenticated attach
//! request to the local mirrord daemon. The daemon uses [`DbPortForwardIdentity`] as the registry
//! key: it adds the requesting session to an existing [`ManagedForward`] when the key matches, or
//! creates a new agent connection, local TCP listener, and forwarding task when it does not.
//!
//! The session IDs in a managed forward are ownership claims, not active database connections.
//! When the daemon's session monitor observes that an intproxy has disappeared, it calls
//! [`release_session`]. Removing the last claim drops the registry entry and aborts its forwarding
//! task, while forwards still claimed by another session remain available.

use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{Json, extract::State, http::StatusCode};
use mirrord_analytics::AnalyticsReporter;
use mirrord_config::LayerConfig;
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::server::AppState;
use crate::{
    config::RemoteAddr,
    internal_proxy::connect_and_ping,
    port_forward::{self, PortForwarder},
};

/// Complete registry key for deciding whether two sessions may share a DB branch forward.
///
/// Requests share a local listener only when their cluster context, namespace, branch ID, and
/// resolved remote endpoint are all equal. Including both the logical branch identity and physical
/// endpoint prevents similarly named branches in different clusters or namespaces from colliding.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct DbPortForwardIdentity {
    pub(crate) kube_context: Option<String>,
    pub(crate) namespace: Option<String>,
    pub(crate) db_id: String,
    pub(crate) remote_host: String,
    pub(crate) remote_port: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DbPortForwardAttachRequest {
    pub(crate) session_id: String,
    pub(crate) identity: DbPortForwardIdentity,
    pub(crate) config: Box<LayerConfig>,
    pub(crate) connect_info: AgentConnectInfo,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DbPortForwardAttachResponse {
    pub(crate) local: SocketAddr,
}

pub(crate) type DbPortForwards = Arc<Mutex<HashMap<DbPortForwardIdentity, ManagedForward>>>;

/// One daemon-owned local listener and the mirrord sessions that currently claim it.
///
/// `sessions` tracks session ownership, rather than individual TCP or database connections. The
/// `task` owns the running [`PortForwarder`]; removing this value from [`DbPortForwards`] invokes
/// [`Drop`], aborting that task and closing the listener after the final session claim is released.
pub(crate) struct ManagedForward {
    local: SocketAddr,
    sessions: HashSet<String>,
    task: JoinHandle<()>,
}

impl Drop for ManagedForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Attaches a registered mirrord session to the forward identified by the request.
///
/// A session must already be visible to the daemon's session monitor; otherwise this returns
/// `409 Conflict` so the intproxy can retry after the filesystem watcher catches up. A finished
/// forward is discarded before lookup. A healthy matching forward gains another session claim and
/// returns its existing local address, while a missing forward gets a new agent connection, local
/// TCP listener, and forwarding task.
///
/// The forward registry remains locked across session validation and claim insertion. Removal first
/// deletes the session, releases the session lock, and then waits for the forward registry. Thus an
/// attach either sees that the session is gone or inserts its claim before removal scans forwards;
/// it cannot leave an orphaned claim.
pub(crate) async fn attach(
    State(state): State<AppState>,
    Json(request): Json<DbPortForwardAttachRequest>,
) -> Result<Json<DbPortForwardAttachResponse>, (StatusCode, String)> {
    let mut forwards = state.db_portforwards.lock().await;
    if state.shutdown.is_cancelled() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "local mirrord daemon is shutting down".to_owned(),
        ));
    }
    // Session removal deletes the session and then takes the forward lock. Holding the forward
    // lock while checking registration means removal cannot miss a concurrently added claim.
    let sessions = state.sessions.read().await;
    if !sessions.contains_key(&request.session_id) {
        return Err((
            StatusCode::CONFLICT,
            "mirrord session is not registered with the daemon yet".to_owned(),
        ));
    }
    drop(sessions);
    if forwards
        .get(&request.identity)
        .is_some_and(|forward| forward.task.is_finished())
    {
        forwards.remove(&request.identity);
    }

    if let Some(forward) = forwards.get_mut(&request.identity) {
        forward.sessions.insert(request.session_id);
        return Ok(Json(DbPortForwardAttachResponse {
            local: forward.local,
        }));
    }

    let identity = request.identity;
    let (_signal, watch) = drain::channel();
    let mut analytics = AnalyticsReporter::only_error(
        false,
        Default::default(),
        watch,
        uuid::Uuid::nil(),
        Some(request.config.key.as_str().to_owned()),
    );
    let mut agent = connect_and_ping(&request.config, request.connect_info, &mut analytics)
        .await
        .map_err(internal_error)?;
    let remote = identity
        .remote_host
        .parse::<Ipv4Addr>()
        .map(RemoteAddr::Ip)
        .unwrap_or_else(|_| RemoteAddr::Hostname(identity.remote_host.clone()));
    let agent_tx = agent.connection.tx_handle();
    let incoming = agent.connection.split_incoming(64, |_| true);
    let mut forwarder = PortForwarder::new(
        agent_tx,
        incoming,
        [(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            (remote, identity.remote_port),
        )],
        None,
    )
    .await
    .map_err(forward_error)?;
    let local = forwarder
        .listeners()
        .next()
        .map(|(local, _)| local)
        .expect("a DB branch forward always creates one listener");
    let log_identity = identity.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = forwarder.run().await {
            error!(?error, ?log_identity, "DB branch port forward stopped");
        }
    });

    info!(?identity, %local, "created daemon-owned DB branch port forward");
    forwards.insert(
        identity,
        ManagedForward {
            local,
            sessions: HashSet::from([request.session_id]),
            task,
        },
    );

    Ok(Json(DbPortForwardAttachResponse { local }))
}

/// Atomically cancels the daemon if no sessions still claim DB branch forwards.
///
/// This uses the same registry lock as [`attach`], so a new claim cannot appear between the safety
/// check and cancellation. Once cancelled, later attachment attempts are rejected.
pub(crate) async fn request_daemon_shutdown(
    registry: &DbPortForwards,
    shutdown: &CancellationToken,
) -> Result<(), Vec<String>> {
    let forwards = registry.lock().await;
    let mut sessions: Vec<_> = forwards
        .values()
        .flat_map(|forward| forward.sessions.iter().cloned())
        .collect();
    sessions.sort_unstable();
    sessions.dedup();

    if sessions.is_empty() {
        shutdown.cancel();
        Ok(())
    } else {
        Err(sessions)
    }
}

pub(crate) async fn release_session(session_id: &str, state: &AppState) {
    release_session_from(session_id, &state.db_portforwards).await;
}

async fn release_session_from(session_id: &str, registry: &DbPortForwards) {
    let mut forwards = registry.lock().await;
    forwards.retain(|identity, forward| {
        forward.sessions.remove(session_id);
        let keep = !forward.sessions.is_empty();
        if !keep {
            info!(?identity, "stopping unclaimed DB branch port forward");
        }
        keep
    });
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn forward_error(error: port_forward::PortForwardError) -> (StatusCode, String) {
    internal_error(error)
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use super::*;

    #[tokio::test]
    async fn forward_stops_only_after_its_last_session_is_released() {
        let identity = DbPortForwardIdentity {
            kube_context: Some("test".to_owned()),
            namespace: Some("default".to_owned()),
            db_id: "db".to_owned(),
            remote_host: "database".to_owned(),
            remote_port: 5432,
        };
        let task = tokio::spawn(pending());
        let abort = task.abort_handle();
        let registry = DbPortForwards::default();
        registry.lock().await.insert(
            identity,
            ManagedForward {
                local: SocketAddr::from((Ipv4Addr::LOCALHOST, 1234)),
                sessions: HashSet::from(["one".to_owned(), "two".to_owned()]),
                task,
            },
        );

        release_session_from("one", &registry).await;
        assert_eq!(registry.lock().await.len(), 1);
        assert!(!abort.is_finished());

        release_session_from("two", &registry).await;
        assert!(registry.lock().await.is_empty());
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
    }

    #[tokio::test]
    async fn daemon_shutdown_is_blocked_until_all_forward_claims_are_released() {
        let identity = DbPortForwardIdentity {
            kube_context: Some("test".to_owned()),
            namespace: Some("default".to_owned()),
            db_id: "db".to_owned(),
            remote_host: "database".to_owned(),
            remote_port: 5432,
        };
        let registry = DbPortForwards::default();
        registry.lock().await.insert(
            identity,
            ManagedForward {
                local: SocketAddr::from((Ipv4Addr::LOCALHOST, 1234)),
                sessions: HashSet::from(["two".to_owned(), "one".to_owned()]),
                task: tokio::spawn(pending()),
            },
        );
        let shutdown = CancellationToken::new();

        assert_eq!(
            request_daemon_shutdown(&registry, &shutdown).await,
            Err(vec!["one".to_owned(), "two".to_owned()])
        );
        assert!(!shutdown.is_cancelled());

        release_session_from("one", &registry).await;
        release_session_from("two", &registry).await;
        assert_eq!(request_daemon_shutdown(&registry, &shutdown).await, Ok(()));
        assert!(shutdown.is_cancelled());
    }
}
