//! DNS selector for Unix layer
//!
//! Re-exports the shared DNS selector from layer-lib and provides Unix-specific extensions.

// Re-export shared DNS selector from layer-lib
pub use mirrord_layer_lib::dns_selector::{CheckQueryResult, DnsSelector};
use tracing::Level;

use crate::detour::{Bypass, Detour};

/// Check DNS query using the Unix-specific behavior
#[tracing::instrument(level = Level::DEBUG, ret)]
pub fn check_query(selector: &DnsSelector, node: &str, port: u16) -> Detour<()> {
    match selector.check_query_result(node, port) {
        CheckQueryResult::Local => Detour::Bypass(Bypass::LocalDns),
        CheckQueryResult::Remote => Detour::Success(()),
    }
}
