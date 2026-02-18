use std::ops::Not;
/// Windows supported subset of LayerSetup
/// this will fill up over time
/// until it becomes layer's LayerSetup
use std::{collections::HashSet, net::SocketAddr};

use mirrord_config::{
    LayerConfig,
    feature::network::{
        NetworkConfig,
        incoming::{IncomingConfig, IncomingMode as ConfigIncomingMode},
        outgoing::OutgoingConfig,
    },
    target::Target,
};
use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    Port,
    tcp::{HttpFilter, MirrorType, StealType},
};

use crate::{
    file::{filter::FileFilter, mapper::FileRemapper},
    socket::{DnsSelector, OutgoingSelector},
};

/// Helper trait for network hook decisions
pub trait NetworkHookConfig {
    fn requires_incoming_hooks(&self) -> bool;
    fn requires_outgoing_hooks(&self) -> bool;
    fn requires_tcp_hooks(&self) -> bool;
    fn requires_udp_hooks(&self) -> bool;
}

impl NetworkHookConfig for NetworkConfig {
    fn requires_incoming_hooks(&self) -> bool {
        self.incoming.mode != ConfigIncomingMode::Off
    }

    fn requires_outgoing_hooks(&self) -> bool {
        self.outgoing.tcp || self.outgoing.udp
    }

    fn requires_tcp_hooks(&self) -> bool {
        self.outgoing.tcp
    }

    fn requires_udp_hooks(&self) -> bool {
        self.outgoing.udp
    }
}

/// Windows supported layer setup.
/// Contains [`LayerConfig`] and derived from it structs, which are used in multiple places across
/// the layer.
#[derive(Debug)]
pub struct LayerSetup {
    config: LayerConfig,
    file_filter: FileFilter,
    file_remapper: FileRemapper,
    outgoing_selector: OutgoingSelector,
    dns_selector: DnsSelector,
    proxy_address: SocketAddr,
    incoming_mode: IncomingMode,
    local_hostname: bool,
}

impl LayerSetup {
    pub fn new(mut config: LayerConfig, proxy_address: SocketAddr, local_hostname: bool) -> Self {
        let file_filter = FileFilter::new(config.feature.fs.clone());
        let file_remapper =
            FileRemapper::new(config.feature.fs.mapping.clone().unwrap_or_default());

        let outgoing_selector = OutgoingSelector::new(&config.feature.network.outgoing);

        let dns_selector = DnsSelector::from(&config.feature.network.dns);

        let incoming_mode = IncomingMode::new(&mut config.feature.network.incoming);
        tracing::info!(?incoming_mode, ?config, "incoming has changed");
        Self {
            config,
            file_filter,
            file_remapper,
            outgoing_selector,
            dns_selector,
            proxy_address,
            incoming_mode,
            local_hostname,
        }
    }

    pub fn layer_config(&self) -> &LayerConfig {
        &self.config
    }

    pub fn file_filter(&self) -> &FileFilter {
        &self.file_filter
    }

    pub fn file_remapper(&self) -> &FileRemapper {
        &self.file_remapper
    }

    pub fn incoming_config(&self) -> &IncomingConfig {
        &self.config.feature.network.incoming
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

    pub fn incoming_mode(&self) -> &IncomingMode {
        &self.incoming_mode
    }

    pub fn local_hostname(&self) -> bool {
        self.local_hostname
    }

    // Hook control methods for configuration-based hook enablement

    /// Check if file system hooks should be enabled based on configuration
    pub fn fs_hooks_enabled(&self) -> bool {
        use mirrord_config::feature::fs::FsModeConfig;
        !matches!(self.config.feature.fs.mode, FsModeConfig::Local)
    }

    /// Check if socket hooks should be enabled based on configuration
    pub fn socket_hooks_enabled(&self) -> bool {
        self.config.feature.network.incoming.mode != ConfigIncomingMode::Off
            || self.config.feature.network.outgoing.tcp
            || self.config.feature.network.outgoing.udp
    }

    /// Check if DNS hooks should be enabled based on configuration
    pub fn dns_hooks_enabled(&self) -> bool {
        self.config.feature.network.dns.enabled
    }

    /// Check if process hooks should be enabled (always true for Windows)
    pub fn process_hooks_enabled(&self) -> bool {
        // Process hooks always needed for Windows DLL injection
        true
    }

    /// Get access to experimental configuration
    pub fn experimental_features(&self) -> &mirrord_config::experimental::ExperimentalConfig {
        &self.config.experimental
    }

    /// Get access to file system configuration
    pub fn fs_config(&self) -> &mirrord_config::feature::fs::FsConfig {
        &self.config.feature.fs
    }

    /// Get access to network configuration
    pub fn network_config(&self) -> &mirrord_config::feature::network::NetworkConfig {
        &self.config.feature.network
    }
}

/// Settings for handling HTTP feature.
#[derive(Debug)]
pub struct HttpSettings {
    /// The HTTP filter to use.
    pub filter: HttpFilter,
    /// Ports to filter HTTP on. `None` means we filter on all ports.
    pub ports: Option<HashSet<Port>>,
}

#[derive(Debug)]
pub struct IncomingMode {
    pub steal: bool,
    pub http_settings: Option<HttpSettings>,
}

impl IncomingMode {
    /// Creates a new instance from the given [`IncomingConfig`].
    /// # Params
    ///
    /// * `config` - [`IncomingConfig`] is taken as `&mut` due to `add_probe_ports_to_http_ports`.
    fn new(config: &mut IncomingConfig) -> Self {
        let http_settings = config.http_filter.is_filter_set().then(|| {
            let ports = config
                .http_filter
                .ports
                .as_ref()
                .cloned()
                .map(HashSet::from);

            let filter = config
                .http_filter
                .as_protocol_http_filter()
                .expect("invalid HTTP filter expression");

            HttpSettings { filter, ports }
        });

        Self {
            steal: config.is_steal(),
            http_settings,
        }
    }

    /// Returns [`PortSubscription`] request to be used for the given port.
    pub fn subscription(&self, port: Port) -> PortSubscription {
        if self.steal {
            let steal_type = match &self.http_settings {
                None => StealType::All(port),
                Some(settings) => {
                    if settings
                        .ports
                        .as_ref()
                        .is_some_and(|p| p.contains(&port).not())
                    {
                        StealType::All(port)
                    } else {
                        StealType::FilteredHttpEx(port, settings.filter.clone())
                    }
                }
            };
            PortSubscription::Steal(steal_type)
        } else {
            let mirror_type = match &self.http_settings {
                None => MirrorType::All(port),
                Some(settings) => {
                    if settings
                        .ports
                        .as_ref()
                        .is_some_and(|p| p.contains(&port).not())
                    {
                        MirrorType::All(port)
                    } else {
                        MirrorType::FilteredHttp(port, settings.filter.clone())
                    }
                }
            };
            PortSubscription::Mirror(mirror_type)
        }
    }
}
