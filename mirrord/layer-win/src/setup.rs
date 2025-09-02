//! Setup and configuration for layer-win
//!
//! This module provides the setup functionality similar to the Unix layer,
//! including outgoing selector and DNS selector for filtering logic.

use mirrord_config::LayerConfig;
pub use mirrord_layer_lib::dns_selector::DnsSelector;
// Re-export common types for compatibility
pub use mirrord_layer_lib::socket::{ConnectionThrough, OutgoingSelector};
use tracing::debug;
use std::net::SocketAddr;
use mirrord_config::MIRRORD_LAYER_INTPROXY_ADDR;
use mirrord_config::feature::network::outgoing::OutgoingConfig;
use mirrord_config::target::Target;

/// Global setup structure for layer-win
pub struct LayerSetup {
    config: LayerConfig,
    outgoing_selector: OutgoingSelector,
    dns_selector: DnsSelector,
    proxy_address: SocketAddr,
}

impl LayerSetup {
    /// Initialize the layer setup from configuration
    pub fn new(config: LayerConfig) -> Self {
        debug!("Creating LayerSetup for Windows layer");
        // debugbreak winapi
        // unsafe { winapi::um::debugapi::DebugBreak(); }

        println!("{:?}", config.feature.network.outgoing);
        let outgoing_selector = OutgoingSelector::new(&config.feature.network.outgoing);
        let dns_selector = DnsSelector::new(&config.feature.network.dns);

        let proxy_address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
            .expect("missing internal proxy address")
            .parse::<SocketAddr>()
            .expect("malformed internal proxy address");

        debug!("Created LayerSetup with outgoing_selector and dns_selector");

        Self {
            config,
            outgoing_selector,
            dns_selector,
            proxy_address,
        }
    }
    
    pub fn layer_config(&self) -> &LayerConfig {
        &self.config
    }

    pub fn outgoing_config(&self) -> &OutgoingConfig {
        &self.config.feature.network.outgoing
    }

    pub fn remote_dns_enabled(&self) -> bool {
        self.config.feature.network.dns.enabled
    }
    
    pub fn targetless(&self) -> bool {
        self.config
            .target
            .path
            .as_ref()
            .map(|path| matches!(path, Target::Targetless))
            .unwrap_or(true)
    }

    pub fn outgoing_selector(&self) -> &OutgoingSelector {
        &self.outgoing_selector
    }

    pub fn dns_selector(&self) -> &DnsSelector {
        &self.dns_selector
    }

    pub fn proxy_address(&self) -> SocketAddr {
        self.proxy_address
    }
    
}
