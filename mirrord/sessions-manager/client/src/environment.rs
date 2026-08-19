//! Definitions of environment variables used to configure sessions-manager connections.
//!
//! If you want to add some more, please do it here.

use crate::error::SessionsManagerClientError;

/// Defines the target within the tenant context connecting to Sessions Manager.
/// remote only, local one is defined in `ServerlessTarget.sessions_manager_service`
pub const REMOTE_SERVICE_ENV: &str = "MIRRORD_REMOTE_SERVICE";

/// Defines the replica identity of the workload-companion agent.
/// remote only, local one is defined in `ServerlessTarget.sessions_manager_target_replica_id`
pub const REMOTE_SERVICE_REPLICA_ENV: &str = "MIRRORD_REMOTE_SERVICE_REPLICA";

/// Defines the environment name for sessions-manager registrations.
/// shared - same env var for local and remote sides, local one can also be set through
/// `TargetConfig.namespace`
pub const REMOTE_ENVIRONMENT_ENV: &str = "MIRRORD_TARGET_NAMESPACE";

fn read_env(name: &str) -> Result<String, SessionsManagerClientError> {
    std::env::var(name)
        .inspect_err(|_| tracing::debug!(env = name, "missing env var"))
        .map_err(Into::into)
}

pub fn sessions_manager_service() -> Result<String, SessionsManagerClientError> {
    read_env(REMOTE_SERVICE_ENV)
}

pub fn sessions_manager_replica_id() -> Result<String, SessionsManagerClientError> {
    read_env(REMOTE_SERVICE_REPLICA_ENV).or_else(|_| read_env("HOSTNAME"))
}

pub fn sessions_manager_environment() -> Option<String> {
    read_env(REMOTE_ENVIRONMENT_ENV).ok()
}
