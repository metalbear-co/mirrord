//! Definitions of environment variables used to configure sessions-manager connections.
//!
//! If you want to add some more, please do it here.

use crate::error::SessionsManagerClientError;

/// Defines the customer connecting to Sessions Manager
pub const TENANT_ID: &str = "MIRRORD_SM_TENANT_ID";

/// Defines the target within the tenant context connecting to Sessions Manager
pub const TARGET_ID: &str = "MIRRORD_SM_TARGET_ID";

/// Defines the specific session connecting to the target,
/// i.e. cli / intproxy / agent, of a client.
pub const SESSION_ID: &str = "MIRRORD_SM_SESSION_ID";

pub fn sessions_manager_room_id() -> Result<String, SessionsManagerClientError> {
    let tenant_id = std::env::var(TENANT_ID).map_err(|err| {
        tracing::error!("missing {TENANT_ID}");
        err
    })?;
    let target_id = std::env::var(TARGET_ID).map_err(|err| {
        tracing::error!("missing {TENANT_ID}");
        err
    })?;
    Ok(format!("{tenant_id}/{target_id}"))
}
