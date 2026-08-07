//! Session-role classification for the Windows layer.
//!
//! This sits in `layer-lib` for one reason. That reason is dependency direction.
//! It is read by `sync::for_child` and by the crash report in `utils-win`.
//!
//! It reads mirrord's own child-inheritance variables. So it is mirrord-domain, not a generic
//! Windows utility.

use std::env;

use super::execution::{
    MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID, MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID,
    MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR,
};

/// The role this process plays in the mirrord session.
#[derive(Debug, Clone)]
pub enum SessionRole {
    /// A top-level process. No child-inheritance variables are present.
    Parent,
    /// A hooked child. It inherited session state through the environment.
    Child {
        parent_pid: u32,
        layer_id: u64,
        proxy_addr: String,
    },
    /// The child-inheritance variables are partial or unparseable.
    ///
    /// This is a red flag. It points at environment-block truncation or corruption.
    MalformedEnv { detail: String },
}

impl SessionRole {
    /// Returns a short label for the role.
    pub fn label(&self) -> &'static str {
        match self {
            SessionRole::Parent => "parent",
            SessionRole::Child { .. } => "child",
            SessionRole::MalformedEnv { .. } => "malformed-env",
        }
    }
}

/// Classifies the current process from its environment.
///
/// The classification reads the child-inheritance variables. A partial or unparseable set is
/// treated as malformed.
///
/// # Returns
///
/// The detected [`SessionRole`].
pub fn session_role() -> SessionRole {
    let parent = env::var(MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID).ok();
    let layer = env::var(MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID).ok();
    let proxy = env::var(MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR).ok();

    match (parent, layer, proxy) {
        (None, None, None) => SessionRole::Parent,
        (Some(parent), Some(layer), Some(proxy)) => {
            match (parent.parse::<u32>(), layer.parse::<u64>()) {
                (Ok(parent_pid), Ok(layer_id)) => SessionRole::Child {
                    parent_pid,
                    layer_id,
                    proxy_addr: proxy,
                },
                _ => SessionRole::MalformedEnv {
                    detail: format!(
                        "unparseable child env (parent_pid={parent:?}, layer_id={layer:?})"
                    ),
                },
            }
        }
        (parent, layer, proxy) => SessionRole::MalformedEnv {
            detail: format!(
                "partial child env (parent_pid={}, layer_id={}, proxy_addr={})",
                parent.map_or("missing", |_| "present"),
                layer.map_or("missing", |_| "present"),
                proxy.map_or("missing", |_| "present"),
            ),
        },
    }
}
