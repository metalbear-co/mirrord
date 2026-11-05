//! What a session started with `share` asks the operator for when it connects.
//!
//! The operator creates the share link during that connect, and serves the link back to the
//! session's owner in the `share` field of the session's `sessions` view (`Session::share` in the
//! `crd` module), where the CLI reads the URL before it starts the application, to print it.

#[cfg(feature = "client")]
use std::ops::Not;

use serde::{Deserialize, Serialize};

/// The `reason` of the Kubernetes `Status` the operator answers with when it can't share a
/// session: the link already exists, the cluster has no share-ingress, or the session uses
/// something sharing doesn't support. Its `message` is written for the user, so clients show it
/// alone instead of wrapping it in advice about reaching the operator. An older operator answers
/// these with another `reason`, and clients treat them as any other failed operator request.
pub const LOCAL_SHARE_REJECTED_REASON: &str = "LocalShareRejected";

/// What the client asks the operator to share when it connects with `share`. It is serialized
/// into the connect URL, so its fields are a contract with the operator.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct LocalShareRequest {
    /// The port the user listed first in `feature.network.incoming.ports`, or `None` when the
    /// user listed none and the operator picks the only port the target container declares.
    pub entry_port: Option<u16>,
    /// `feature.network.incoming.ignore_ports`, so the operator applies the same exclusions when
    /// it picks the port.
    #[serde(default)]
    pub ignored_ports: Vec<u16>,
    /// `feature.network.incoming.http_filter.ports`, so the operator applies the same scope when
    /// it picks the port.
    #[serde(default)]
    pub http_filter_ports: Option<Vec<u16>>,
}

#[cfg(feature = "client")]
impl LocalShareRequest {
    /// The request of a session started with `share`, or `None` for any other session.
    pub fn from_config(config: &mirrord_config::LayerConfig) -> Option<Self> {
        if config.share.not() {
            return None;
        }
        let incoming = &config.feature.network.incoming;
        let mut ignored_ports: Vec<_> = incoming.ignore_ports.iter().copied().collect();
        ignored_ports.sort_unstable();
        let http_filter_ports = incoming.http_filter.ports.as_ref().map(|ports| {
            let mut ports: Vec<_> = ports.iter().copied().collect();
            ports.sort_unstable();
            ports.dedup();
            ports
        });
        Some(Self {
            entry_port: incoming.first_configured_port,
            ignored_ports,
            http_filter_ports,
        })
    }
}
