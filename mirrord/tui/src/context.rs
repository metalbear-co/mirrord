use std::sync::Arc;

use kube::Client;
use tokio::sync::{Notify, watch};

use crate::{local_sessions::LocalSessions, scope::Scope, telemetry::Telemetry};

/// The application context.
///
/// Fields are public for direct watch access; the accessor methods are the
/// same state for call sites that prefer a receiver to borrow from.
#[derive(Clone)]
pub struct Context {
    /// The current scope.
    pub scope: watch::Receiver<Scope>,
    /// The current client, if connected.
    pub client: watch::Receiver<Option<anyhow::Result<Client>>>,
    /// The current local sessions, if any.
    pub local_sessions: watch::Receiver<Option<LocalSessions>>,
    /// Whether only local session resources should be shown.
    pub local_only: watch::Receiver<bool>,
    /// Scheduled redraw request for the application.
    pub redraw: Arc<Notify>,
    /// Anonymous usage reporting, which does nothing unless the caller asked for it.
    pub telemetry: Telemetry,
}

impl Context {
    /// Creates a new instance.
    pub fn new(
        scope: watch::Receiver<Scope>,
        client: watch::Receiver<Option<anyhow::Result<Client>>>,
        local_sessions: watch::Receiver<Option<LocalSessions>>,
        local_only: watch::Receiver<bool>,
        redraw: Arc<Notify>,
        telemetry: Telemetry,
    ) -> Self {
        Self {
            scope,
            client,
            local_sessions,
            local_only,
            redraw,
            telemetry,
        }
    }

    /// The current scope.
    pub fn scope(&mut self) -> &mut watch::Receiver<Scope> {
        &mut self.scope
    }

    /// The current client, if connected.
    pub fn client(&mut self) -> &mut watch::Receiver<Option<anyhow::Result<Client>>> {
        &mut self.client
    }

    /// The current local sessions, if any.
    #[allow(unused, reason = "Nothing uses this yet.")]
    pub fn local_sessions(&mut self) -> &mut watch::Receiver<Option<LocalSessions>> {
        &mut self.local_sessions
    }

    /// Whether only local session resources should be shown.
    #[allow(unused, reason = "Nothing uses this yet.")]
    pub fn local_only(&mut self) -> &mut watch::Receiver<bool> {
        &mut self.local_only
    }

    /// Requests a redraw from the application.
    pub fn request_redraw(&self) {
        self.redraw.notify_one();
    }
}
