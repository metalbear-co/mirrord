/// Windows supported subset of LayerSetup
/// this will fill up over time
/// until it becomes layer's LayerSetup
use std::{net::SocketAddr, sync::OnceLock};

use anyhow;
use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, feature::network::outgoing::OutgoingConfig,
    target::Target,
};

use crate::{
    setup::CONFIG,
    socket::{DnsSelector, OutgoingSelector},
};

static SETUP: OnceLock<LayerSetup> = OnceLock::new();

pub fn init_setup(config: LayerConfig) -> anyhow::Result<()> {
    let state = LayerSetup::new(config);
    SETUP.set(state).expect("Failed to set layer setup");
    Ok(())
}

pub fn layer_setup() -> &'static LayerSetup {
    SETUP.get().expect("LayerSetup is not initialized")
}

/// Windows supported layer setup.
/// Contains [`LayerConfig`] and derived from it structs, which are used in multiple places across
/// the layer.
#[derive(Debug)]
pub struct LayerSetup {
    // config: LayerConfig,
    outgoing_selector: OutgoingSelector,
    dns_selector: DnsSelector,
    proxy_address: SocketAddr,
    local_hostname: bool,
}

impl LayerSetup {
    pub fn new(config: LayerConfig) -> Self {
        let outgoing_selector = OutgoingSelector::new(&config.feature.network.outgoing);

        let dns_selector = DnsSelector::from(&config.feature.network.dns);

        let proxy_address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
            .expect("missing internal proxy address")
            .parse::<SocketAddr>()
            .expect("malformed internal proxy address");

        let local_hostname = !config.feature.hostname;

        CONFIG.set(config).expect("Failed to set layer config");

        Self {
            // config,
            outgoing_selector,
            dns_selector,
            proxy_address,
            local_hostname,
        }
    }

    pub fn layer_config(&self) -> &LayerConfig {
        &CONFIG.get().expect("Layer config not initialized")
    }

    pub fn outgoing_config(&self) -> &OutgoingConfig {
        &self.layer_config().feature.network.outgoing
    }

    pub fn remote_dns_enabled(&self) -> bool {
        self.layer_config().feature.network.dns.enabled
    }

    pub fn targetless(&self) -> bool {
        self.layer_config()
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

    pub fn local_hostname(&self) -> bool {
        self.local_hostname
    }
}
