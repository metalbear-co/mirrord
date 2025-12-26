use std::{collections::HashSet, net::SocketAddr, str::FromStr, sync::OnceLock};

use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR,
    experimental::ExperimentalConfig,
    feature::{
        env::EnvConfig,
        fs::{FsConfig, FsModeConfig, READONLY_FILE_BUFFER_DEFAULT},
        network::{
            NetworkConfig,
            incoming::{
                IncomingConfig, IncomingMode as ConfigIncomingMode,
                http_filter::{BodyFilter, HttpFilterConfig, InnerFilter},
            },
            outgoing::OutgoingConfig,
        },
    },
    target::Target,
};
use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    Port,
    tcp::{Filter, HttpBodyFilter, HttpFilter, HttpMethodFilter, MirrorType, StealType},
};
use regex::RegexSet;

use crate::{
    debugger_ports::DebuggerPorts,
    file::{filter::FileFilter, mapper::FileRemapper},
    socket::{DnsSelector, OutgoingSelector},
    trace_only::{is_trace_only_mode, modify_config_for_trace_only},
};

static SETUP: OnceLock<LayerSetup> = OnceLock::new();

pub fn setup() -> &'static LayerSetup {
    SETUP.get().expect("layer is not initialized")
}

/// Initialized LayerSetup from LayerConfig
pub fn init_layer_setup(mut config: LayerConfig, sip_only: bool) {
    // Check if we're in trace only mode (no agent)
    let trace_only = is_trace_only_mode();

    if sip_only {
        // we need to hook file access to patch path to our temp bin.
        config.feature.fs = FsConfig {
            mode: FsModeConfig::Local,
            read_write: None,
            read_only: None,
            local: None,
            not_found: None,
            mapping: None,
            readonly_file_buffer: READONLY_FILE_BUFFER_DEFAULT,
        };
    } else {
        if config.target.path.is_none() && config.feature.fs.mode.ne(&FsModeConfig::Local) {
            // Use localwithoverrides on targetless regardless of user config, unless fs-mode is
            // already set to local.
            config.feature.fs.mode = FsModeConfig::LocalWithOverrides;
        }

        // Disable all features that require the agent
        if trace_only {
            modify_config_for_trace_only(&mut config);
        }
    }

    // init setup
    let debugger_ports = DebuggerPorts::from_env();
    let local_hostname = sip_only || trace_only || !config.feature.hostname;
    let state = LayerSetup::new(config, debugger_ports, local_hostname);
    SETUP.set(state).unwrap();
}
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

    pub fn network_config(&self) -> &NetworkConfig {
        &self.config.feature.network
    }

    pub fn incoming_config(&self) -> &IncomingConfig {
        &self.network_config().incoming
    }

    pub fn outgoing_config(&self) -> &OutgoingConfig {
        &self.network_config().outgoing
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

    // Hook control methods for configuration-based hook enablement

    /// Check if file system hooks should be enabled based on configuration
    pub fn fs_hooks_enabled(&self) -> bool {
        !matches!(self.config.feature.fs.mode, FsModeConfig::Local)
    }

    /// Check if socket hooks should be enabled based on configuration
    pub fn socket_hooks_enabled(&self) -> bool {
        self.config.feature.network.incoming.mode != ConfigIncomingMode::Off
            || self.outgoing_config().tcp
            || self.outgoing_config().udp
    }

    /// Check if DNS hooks should be enabled based on configuration
    pub fn dns_hooks_enabled(&self) -> bool {
        self.remote_dns_enabled()
    }

    /// Check if process hooks should be enabled (always true for Windows)
    pub fn process_hooks_enabled(&self) -> bool {
        // Process hooks always needed for Windows DLL injection
        true
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
    pub fn new(config: &mut IncomingConfig) -> Self {
        let http_settings = config.http_filter.is_filter_set().then(|| {
            let ports = config
                .http_filter
                .ports
                .get_or_insert_default()
                .iter()
                .copied()
                .collect();

            let filter = Self::parse_http_filter(&config.http_filter);

            HttpSettings { filter, ports }
        });

        Self {
            steal: config.is_steal(),
            http_settings,
        }
    }

    fn parse_body_filter(filter: &BodyFilter) -> HttpBodyFilter {
        match filter {
            BodyFilter::Json { query, matches } => HttpBodyFilter::Json {
                query: query.clone(),
                matches: Filter::new(matches.clone())
                    .expect("invalid json body filter `matches` string"),
            },
        }
    }

    fn parse_http_filter(http_filter_config: &HttpFilterConfig) -> HttpFilter {
        match http_filter_config {
            HttpFilterConfig {
                path_filter: Some(path),
                header_filter: None,
                method_filter: None,
                body_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => HttpFilter::Path(Filter::new(path.into()).expect("invalid filter expression")),

            HttpFilterConfig {
                path_filter: None,
                header_filter: Some(header),
                method_filter: None,
                body_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => HttpFilter::Header(Filter::new(header.into()).expect("invalid filter expression")),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: Some(method),
                body_filter: None,
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
                body_filter: Some(filter),
                all_of: None,
                any_of: None,
                ports: _ports,
            } => HttpFilter::Body(Self::parse_body_filter(filter)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                body_filter: None,
                all_of: Some(filters),
                any_of: None,
                ports: _ports,
            } => Self::make_composite_filter(true, filters),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                body_filter: None,
                all_of: None,
                any_of: Some(filters),
                ports: _ports,
            } => Self::make_composite_filter(false, filters),

            _ => panic!("No HTTP filters specified, this should have been caught earlier"),
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
                InnerFilter::Body(body_filter) => {
                    HttpFilter::Body(Self::parse_body_filter(body_filter))
                }
            })
            .collect();

        HttpFilter::Composite { all, filters }
    }

    /// Returns [`PortSubscription`] request to be used for the given port.
    pub fn subscription(&self, port: Port) -> PortSubscription {
        if self.steal {
            let steal_type = match &self.http_settings {
                None => StealType::All(port),
                Some(settings) => {
                    if settings.ports.contains(&port) {
                        StealType::FilteredHttpEx(port, settings.filter.clone())
                    } else {
                        StealType::All(port)
                    }
                }
            };
            PortSubscription::Steal(steal_type)
        } else {
            let mirror_type = match &self.http_settings {
                None => MirrorType::All(port),
                Some(settings) => {
                    if settings.ports.contains(&port) {
                        MirrorType::FilteredHttp(port, settings.filter.clone())
                    } else {
                        MirrorType::All(port)
                    }
                }
            };
            PortSubscription::Mirror(mirror_type)
        }
    }
}

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
