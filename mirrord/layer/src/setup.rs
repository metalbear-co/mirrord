use std::{collections::HashSet, net::SocketAddr, ops::Not};

use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR,
    experimental::ExperimentalConfig,
    feature::{
        env::EnvConfig,
        fs::FsConfig,
        network::{incoming::IncomingConfig, outgoing::OutgoingConfig},
    },
    target::Target,
};
use mirrord_intproxy_protocol::PortSubscription;
use mirrord_layer_lib::file::{filter::FileFilter, mapper::FileRemapper};
use mirrord_protocol::{
    Port,
    tcp::{HttpFilter, MirrorType, StealType},
};
use regex::RegexSet;

use crate::{
    debugger_ports::DebuggerPorts,
    socket::{OutgoingSelector, dns_selector::DnsSelector},
};

/// Complete layer setup.
/// Contains [`LayerConfig`] and derived from it structs, which are used in multiple places across
/// the layer.
#[derive(Debug)]
pub struct LayerSetup {
    config: LayerConfig,
    file_filter: FileFilter,
    file_remapper: FileRemapper,
    debugger_ports: DebuggerPorts,
    remote_unix_streams: RegexSet,
    outgoing_selector: OutgoingSelector,
    dns_selector: DnsSelector,
    proxy_address: SocketAddr,
    incoming_mode: IncomingMode,
    local_hostname: bool,
    // to be used on macOS to restore env on execv
    #[cfg(target_os = "macos")]
    env_backup: Vec<(String, String)>,
}

impl LayerSetup {
    pub fn new(
        mut config: LayerConfig,
        debugger_ports: DebuggerPorts,
        local_hostname: bool,
    ) -> Self {
        let file_filter = FileFilter::new(config.feature.fs.clone());
        let file_remapper =
            FileRemapper::new(config.feature.fs.mapping.clone().unwrap_or_default());

        let remote_unix_streams = config
            .feature
            .network
            .outgoing
            .unix_streams
            .as_deref()
            .map(RegexSet::new)
            .transpose()
            .expect("invalid unix stream regex set")
            .unwrap_or_default();

        let outgoing_selector = OutgoingSelector::new(&config.feature.network.outgoing);

        let dns_selector = DnsSelector::from(&config.feature.network.dns);

        let proxy_address = std::env::var(MIRRORD_LAYER_INTPROXY_ADDR)
            .expect("missing internal proxy address")
            .parse::<SocketAddr>()
            .expect("malformed internal proxy address");

        let incoming_mode = IncomingMode::new(&mut config.feature.network.incoming);
        tracing::info!(?incoming_mode, ?config, "incoming has changed");
        #[cfg(target_os = "macos")]
        let env_backup = std::env::vars()
            .filter(|(k, _)| k.starts_with("MIRRORD_") || k == "DYLD_INSERT_LIBRARIES")
            .collect();

        Self {
            config,
            file_filter,
            file_remapper,
            debugger_ports,
            remote_unix_streams,
            outgoing_selector,
            dns_selector,
            proxy_address,
            incoming_mode,
            local_hostname,
            #[cfg(target_os = "macos")]
            env_backup,
        }
    }

    pub fn layer_config(&self) -> &LayerConfig {
        &self.config
    }

    pub fn env_config(&self) -> &EnvConfig {
        &self.config.feature.env
    }

    pub fn fs_config(&self) -> &FsConfig {
        &self.config.feature.fs
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

    pub fn experimental(&self) -> &ExperimentalConfig {
        &self.config.experimental
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

    #[cfg(target_os = "macos")]
    pub fn sip_binaries(&self) -> Vec<String> {
        self.config
            .sip_binaries
            .as_deref()
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    }

    #[cfg(target_os = "macos")]
    pub fn skip_patch_binaries(&self) -> Vec<String> {
        self.config.skip_sip.to_vec()
    }

    pub fn is_debugger_port(&self, addr: &SocketAddr) -> bool {
        self.debugger_ports.contains(addr)
    }

    pub fn outgoing_selector(&self) -> &OutgoingSelector {
        &self.outgoing_selector
    }

    pub fn dns_selector(&self) -> &DnsSelector {
        &self.dns_selector
    }

    pub fn remote_unix_streams(&self) -> &RegexSet {
        &self.remote_unix_streams
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

    #[cfg(target_os = "macos")]
    pub fn env_backup(&self) -> &Vec<(String, String)> {
        &self.env_backup
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
