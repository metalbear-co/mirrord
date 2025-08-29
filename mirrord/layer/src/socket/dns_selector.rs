//! DNS selector for Unix layer
//!
//! Re-exports the shared DNS selector from layer-lib and provides Unix-specific extensions.

use mirrord_config::feature::network::dns::DnsConfig;
// Re-export shared DNS selector from layer-lib
pub use mirrord_layer_lib::dns_selector::{CheckQueryResult, DnsSelector};
use tracing::Level;

use crate::detour::{Bypass, Detour};

/// Unix-specific extensions for DnsSelector
impl DnsSelector {
    /// Bypasses queries that should be done locally (Unix-specific method).
    /// Returns Detour<()> which is used by the Unix layer's detour system.
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub fn check_query(&self, node: &str, port: u16) -> Detour<()> {
        match self.check_query_result(node, port) {
            CheckQueryResult::Local => Detour::Bypass(Bypass::LocalDns),
            CheckQueryResult::Remote => Detour::Success(()),
        }
    }
}
