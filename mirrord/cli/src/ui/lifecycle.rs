//! Decides when the local daemon stops, atomically with the work that keeps it running.
//!
//! A daemon started by a mirrord session for its own needs ([`DaemonMode::SessionOwned`]) stops
//! itself once nothing has used it for [`IDLE_SHUTDOWN_TIMEOUT`], so sessions do not leave a
//! daemon behind on the machine forever. A daemon the user started to look at
//! ([`DaemonMode::Persistent`]) runs until `mirrord ui stop`. Explicitly opening the UI against a
//! session-owned daemon promotes it to persistent.
//!
//! Admitting new work and shutting down must never interleave, or a session could register (or
//! claim a DB forward) with a daemon that is about to exit. Every transition is serialized through
//! the DB forward registry lock, the sessions lock, the [`DaemonLifecycle`] state lock, and
//! [`AppState::shutdown`], always acquired in that order:
//!
//! - Shutdown (idle or `mirrord ui stop`) holds the registry lock, a sessions read lock and the
//!   lifecycle state lock while it checks for activity and cancels [`AppState::shutdown`].
//! - Session registration checks cancellation under the sessions write lock, so it either completes
//!   before shutdown looks at the sessions, or sees the cancellation and is rejected.
//! - DB forward attachment checks cancellation under the registry lock.
//! - Promotion checks cancellation under the lifecycle state lock only. That lock is never held
//!   across an `await`, so promotion does not wait behind an attachment that holds the registry
//!   lock while it connects to the agent.
//!
//! Once cancelled, the daemon also fails `ping`, so discovery never hands a draining daemon to a
//! new session.

use std::{
    env,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::{db_portforwards, server::AppState};

/// Environment variable carrying the [`DaemonMode`] from `ui_start` to the spawned daemon process.
pub(super) const MIRRORD_SERVER_MODE_ENV_NAME: &str = "MIRRORD_SPAWNED_SERVER_MODE";

/// How long a [`DaemonMode::SessionOwned`] daemon stays up with nothing using it.
pub(super) const IDLE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// How often [`start_idle_shutdown`] checks whether the daemon is idle.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Who the daemon runs for, which decides whether it may stop itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaemonMode {
    /// Started explicitly by the user (`mirrord ui`, `mirrord wizard`, `mirrord up --ui`). Runs
    /// until `mirrord ui stop`, because the user may be using the UI with no local session.
    Persistent,
    /// Started on demand by a mirrord session. Stops after [`IDLE_SHUTDOWN_TIMEOUT`] without use,
    /// which does not depend on any session exiting cleanly.
    SessionOwned,
}

impl DaemonMode {
    const PERSISTENT: &str = "persistent";
    const SESSION_OWNED: &str = "session-owned";

    pub(super) fn as_env_value(self) -> &'static str {
        match self {
            Self::Persistent => Self::PERSISTENT,
            Self::SessionOwned => Self::SESSION_OWNED,
        }
    }

    /// Reads the mode the daemon was spawned with. Anything unrecognized means persistent, so a
    /// daemon never stops itself unless its starter explicitly asked for that.
    pub(super) fn from_env() -> Self {
        match env::var(MIRRORD_SERVER_MODE_ENV_NAME).as_deref() {
            Ok(Self::SESSION_OWNED) => Self::SessionOwned,
            _ => Self::Persistent,
        }
    }
}

/// Lifecycle state shared by the daemon's routes and background tasks.
pub(crate) struct DaemonLifecycle {
    /// Behind a synchronous lock that is never held across an `await`, see the module docs.
    state: Mutex<LifecycleState>,
}

struct LifecycleState {
    /// Set when the daemon runs in, or was promoted to, [`DaemonMode::Persistent`].
    persistent: bool,
    /// Last time an authenticated request arrived or the daemon was seen busy. Browser tabs poll
    /// the daemon, so this keeps a daemon that is being looked at from stopping.
    last_activity: Instant,
}

impl DaemonLifecycle {
    pub(crate) fn new(mode: DaemonMode) -> Self {
        Self {
            state: Mutex::new(LifecycleState {
                persistent: mode == DaemonMode::Persistent,
                last_activity: Instant::now(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, LifecycleState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn is_persistent(&self) -> bool {
        self.state().persistent
    }

    /// Restarts the idle clock.
    pub(crate) fn touch(&self) {
        self.state().last_activity = Instant::now();
    }

    /// Makes the daemon persistent, so it no longer stops itself when idle.
    ///
    /// Fails when the daemon is already shutting down, in which case the caller must wait for it
    /// to exit and start a new one.
    pub(crate) fn promote(&self, shutdown: &CancellationToken) -> bool {
        let mut state = self.state();
        if shutdown.is_cancelled() {
            return false;
        }
        if !state.persistent {
            state.persistent = true;
            info!("local mirrord daemon promoted to persistent");
        }
        true
    }
}

/// Stops the daemon on `mirrord ui stop`, unless sessions still claim DB branch forwards.
///
/// Returns the IDs of the sessions holding claims when it refuses. See the module docs for why the
/// sessions and lifecycle locks are held across the cancellation.
pub(crate) async fn request_shutdown(state: &AppState) -> Result<(), Vec<String>> {
    let forwards = state.db_portforwards.lock().await;
    let _sessions = state.sessions.read().await;
    let _lifecycle = state.lifecycle.state();

    let claims = db_portforwards::claimed_sessions(&forwards);
    if claims.is_empty() {
        state.shutdown.cancel();
        Ok(())
    } else {
        Err(claims)
    }
}

/// Stops a session-owned daemon that has had nothing to do for `idle_timeout`.
///
/// The daemon is busy while it tracks a session, a session claims a DB forward, or a WebSocket
/// client is connected. Being busy, like any authenticated request (a polling browser tab, or a
/// session pinging the daemon before it registers), restarts the idle clock. Returns whether the
/// daemon is shutting down.
pub(crate) async fn stop_if_idle(state: &AppState, idle_timeout: Duration) -> bool {
    let forwards = state.db_portforwards.lock().await;
    let sessions = state.sessions.read().await;
    let mut lifecycle = state.lifecycle.state();

    if state.shutdown.is_cancelled() {
        return true;
    }
    if lifecycle.persistent {
        return false;
    }

    let busy = !sessions.is_empty()
        || !db_portforwards::claimed_sessions(&forwards).is_empty()
        || state.notify_tx.receiver_count() > 0;
    if busy {
        lifecycle.last_activity = Instant::now();
        return false;
    }

    let idle_for = lifecycle.last_activity.elapsed();
    if idle_for < idle_timeout {
        return false;
    }

    info!(
        idle_secs = idle_for.as_secs(),
        "local mirrord daemon stopping itself: nothing used it"
    );
    state.shutdown.cancel();
    true
}

/// Runs [`stop_if_idle`] periodically until the daemon shuts down or becomes persistent.
pub(crate) fn start_idle_shutdown(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(IDLE_CHECK_INTERVAL) => {}
                _ = state.shutdown.cancelled() => return,
            }

            if stop_if_idle(&state, IDLE_SHUTDOWN_TIMEOUT).await {
                return;
            }
            if state.lifecycle.is_persistent() {
                debug!("idle shutdown disabled: the daemon is persistent");
                return;
            }
        }
    });
}
