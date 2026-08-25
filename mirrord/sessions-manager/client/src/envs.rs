//! Definitions of environment variables used to configure sessions-manager connections.
//!
//! If you want to add some more, please do it here.

use crate::error::SessionsManagerClientError;

/// Defines the target within the tenant context connecting to Sessions Manager.
/// remote only, local one is defined in `ServerlessTarget.sessions_manager_room_id`
pub const REMOTE_SERVICE: &str = "MIRRORD_REMOTE_SERVICE";

/// Defines the replica identity of the workload-companion agent.
/// remote only, local one is defined in `ServerlessTarget.sessions_manager_target_replica_id`
pub const REMOTE_SERVICE_REPLICA: &str = "MIRRORD_REMOTE_SERVICE_REPLICA";

/// Defines the environment namespace for sessions-manager registrations.
/// shared - same env var for local and remote sides, local one can also be set through
/// `TargetConfig.namespace`
pub const REMOTE_TARGET_NAMESPACE: &str = "MIRRORD_TARGET_NAMESPACE";

/// Names the header carrying [`SESSIONS_MANAGER_AUTH_TOKEN`].
/// Only needed when the deployment expects something other than
/// [`DEFAULT_AUTH_HEADER_NAME`].
pub const SESSIONS_MANAGER_AUTH_HEADER: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_HEADER";

/// Shared secret expected by whatever fronts sessions-manager. Setting it is
/// what turns the header on.
pub const SESSIONS_MANAGER_AUTH_TOKEN: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN";

const DEFAULT_AUTH_HEADER_NAME: &str = "x-mirrord-sm-auth";

fn read_env(name: &str) -> Result<String, SessionsManagerClientError> {
    std::env::var(name)
        .inspect_err(|_| {
            tracing::debug!(env = name, "missing env var");
        })
        .map_err(|err| err.into())
}

pub fn sessions_manager_room_id() -> Result<String, SessionsManagerClientError> {
    read_env(REMOTE_SERVICE)
}

pub fn sessions_manager_replica_id() -> Result<String, SessionsManagerClientError> {
    read_env(REMOTE_SERVICE_REPLICA).or_else(|_| read_env("HOSTNAME"))
}

pub fn sessions_manager_namespace() -> Option<String> {
    read_env(REMOTE_TARGET_NAMESPACE).ok()
}

/// Header to attach to every sessions-manager connection, for deployments that
/// put an authenticating proxy or load balancer in front of it.
///
/// [`None`] when no token is set, which is the case for a directly reachable
/// sessions-manager.
pub fn sessions_manager_auth_header() -> Option<(String, String)> {
    let token = read_env(SESSIONS_MANAGER_AUTH_TOKEN).ok()?;
    let name = read_env(SESSIONS_MANAGER_AUTH_HEADER)
        .unwrap_or_else(|_| DEFAULT_AUTH_HEADER_NAME.to_owned());

    Some((name, token))
}
