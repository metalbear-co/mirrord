use std::{collections::HashSet, net::SocketAddr, str::FromStr};

use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR,
    experimental::ExperimentalConfig,
    feature::{
        env::EnvConfig,
        fs::FsConfig,
        network::{
            incoming::{
                IncomingConfig,
                http_filter::{HttpFilterConfig, InnerFilter},
            },
            outgoing::OutgoingConfig,
        },
    },
    target::Target,
};
use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    Port,
    tcp::{Filter, HttpFilter, HttpMethodFilter, MirrorType, StealType},
};
use regex::RegexSet;

use crate::{
    debugger_ports::DebuggerPorts,
    file::{filter::FileFilter, mapper::FileRemapper},
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
    /// Ports to filter HTTP on.
    pub ports: HashSet<Port>,
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
        if !config.is_steal() {
            if config.http_filter.is_filter_set() {
                let ports = config
                    .http_filter
                    .ports
                    .get_or_insert_default()
                    .iter()
                    .copied()
                    .collect();

                let filter = Self::parse_http_filter(&config.http_filter);

                return Self {
                    steal: false,
                    http_settings: Some(HttpSettings { filter, ports }),
                };
            }

            return Self {
                steal: false,
                http_settings: None,
            };
        }

        let ports = config
            .http_filter
            .ports
            .get_or_insert_default()
            .iter()
            .copied()
            .collect();

        // Matching all fields to make this check future-proof.
        let filter = Self::parse_http_filter(&config.http_filter);

        Self {
            steal: true,
            http_settings: Some(HttpSettings { filter, ports }),
        }
    }

    fn parse_http_filter(http_filter_config: &HttpFilterConfig) -> HttpFilter {
        match http_filter_config {
            HttpFilterConfig {
                path_filter: Some(path),
                header_filter: None,
                method_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => HttpFilter::Path(Filter::new(path.into()).expect("invalid filter expression")),

            HttpFilterConfig {
                path_filter: None,
                header_filter: Some(header),
                method_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => HttpFilter::Header(Filter::new(header.into()).expect("invalid filter expression")),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: Some(method),
                all_of: None,
                any_of: None,
                ports: _ports,
            } => HttpFilter::Method(
                HttpMethodFilter::from_str(method).expect("invalid method filter string"),
            ),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                all_of: Some(filters),
                any_of: None,
                ports: _ports,
            } => Self::make_composite_filter(true, filters),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                all_of: None,
                any_of: Some(filters),
                ports: _ports,
            } => Self::make_composite_filter(false, filters),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => Self::make_composite_filter(false, &[]),

            _ => panic!("multiple HTTP filters specified, this is a bug"),
        }
    }

    fn make_composite_filter(all: bool, filters: &[InnerFilter]) -> HttpFilter {
        let filters = filters
            .iter()
            .map(|filter| match filter {
                InnerFilter::Path { path } => {
                    HttpFilter::Path(Filter::new(path.clone()).expect("invalid filter expression"))
                }
                InnerFilter::Header { header } => HttpFilter::Header(
                    Filter::new(header.clone()).expect("invalid filter expression"),
                ),
                InnerFilter::Method { method } => HttpFilter::Method(
                    HttpMethodFilter::from_str(method).expect("invalid method filter string"),
                ),
            })
            .collect();

        HttpFilter::Composite { all, filters }
    }

    /// Returns [`PortSubscription`] request to be used for the given port.
    pub fn subscription(&self, port: Port) -> PortSubscription {
        if self.steal {
            let steal_type = match &self.http_settings {
                None => StealType::All(port),
                Some(settings) => StealType::FilteredHttpEx(port, settings.filter.clone()),
            };
            return PortSubscription::Steal(steal_type);
        }

        let mirror_type = match &self.http_settings {
            None => MirrorType::All(port),
            Some(settings) => MirrorType::FilteredHttp(port, settings.filter.clone()),
        };

        return PortSubscription::Mirror(mirror_type);
    }
}
