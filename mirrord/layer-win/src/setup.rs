//! Setup and configuration for layer-win
//!
//! This module provides the setup functionality similar to the Unix layer,
//! including outgoing selector and DNS selector for filtering logic.

use mirrord_config::LayerConfig;
pub use mirrord_layer_lib::dns_selector::DnsSelector;
// Re-export common types for compatibility
pub use mirrord_layer_lib::socket::{ConnectionThrough, OutgoingSelector};
use tracing::debug;

/// Global setup structure for layer-win
pub struct LayerSetup {
    pub outgoing_selector: OutgoingSelector,
    pub dns_selector: DnsSelector,
}

impl LayerSetup {
    /// Initialize the layer setup from configuration
    pub fn new(config: &LayerConfig) -> Self {
        debug!("Creating LayerSetup for Windows layer");

        let outgoing_selector = OutgoingSelector::new(&config.feature.network.outgoing);
        let dns_selector = DnsSelector::new(&config.feature.network.dns);

        debug!("Created LayerSetup with outgoing_selector and dns_selector");

        Self {
            outgoing_selector,
            dns_selector,
        }
    }
}
