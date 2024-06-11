use std::{collections::HashSet, net::SocketAddr};

use mirrord_config::{
    experimental::ExperimentalConfig,
    feature::{
        env::EnvConfig,
        fs::FsConfig,
        network::{incoming::IncomingConfig, outgoing::OutgoingConfig},
    },
    target::Target,
    util::VecOrSingle,
    LayerConfig,
};
use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    tcp::{Filter, HttpFilter, StealType},
    Port,
};
use regex::RegexSet;

use crate::{debugger_ports::DebuggerPorts, file::filter::FileFilter, socket::OutgoingSelector};

/// Complete layer setup.
/// Contains [`LayerConfig`] and derived from it structs, which are used in multiple places across
/// the layer.
#[derive(Debug)]
pub struct LayerSetup {
    config: LayerConfig,
    file_filter: FileFilter,
    debugger_ports: DebuggerPorts,
    remote_unix_streams: RegexSet,
    outgoing_selector: OutgoingSelector,
    proxy_address: SocketAddr,
    incoming_mode: IncomingMode,
    local_hostname: bool,
    // to be used on macOS to restore env on execv
    #[cfg(target_os = "macos")]
    env_backup: Vec<(String, String)>,
}

impl LayerSetup {
    pub fn new(config: LayerConfig, debugger_ports: DebuggerPorts, local_hostname: bool) -> Self {
        let file_filter = FileFilter::new(config.feature.fs.clone());

        let remote_unix_streams = config
            .feature
            .network
            .outgoing
            .unix_streams
            .as_ref()
            .map(VecOrSingle::as_slice)
            .map(RegexSet::new)
            .transpose()
            .expect("invalid unix stream regex set")
            .unwrap_or_default();

        let outgoing_selector: OutgoingSelector =
            OutgoingSelector::new(&config.feature.network.outgoing);

        let proxy_address = config
            .connect_tcp
            .as_ref()
            .expect("missing internal proxy address")
            .parse()
            .expect("failed to parse internal proxy address");

        let incoming_mode = IncomingMode::new(&config.feature.network.incoming);
        #[cfg(target_os = "macos")]
        let env_backup = std::env::vars()
            .filter(|(k, _)| k.starts_with("MIRRORD_") || k == "DYLD_INSERT_LIBRARIES")
            .collect();

        Self {
            config,
            file_filter,
            debugger_ports,
            remote_unix_streams,
            outgoing_selector,
            proxy_address,
            incoming_mode,
            local_hostname,
            #[cfg(target_os = "macos")]
            env_backup,
        }
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
        self.config.feature.network.dns
    }

    pub fn targetless(&self) -> bool {
        self.config
            .target
            .path
            .as_ref()
            .map(|path| matches!(path, Target::Targetless))
            .unwrap_or(true)
    }

    pub fn sip_binaries(&self) -> Vec<String> {
        self.config
            .sip_binaries
            .clone()
            .map(VecOrSingle::to_vec)
            .unwrap_or_default()
    }

    pub fn is_debugger_port(&self, addr: &SocketAddr) -> bool {
        self.debugger_ports.contains(addr)
    }

    pub fn outgoing_selector(&self) -> &OutgoingSelector {
        &self.outgoing_selector
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

/// HTTP filter used by the layer with the `steal` feature.
#[derive(Debug)]
pub enum StealHttpFilter {
    /// No filter.
    None,
    /// More recent filter (header or path).
    Filter(HttpFilter),
}

/// Settings for handling HTTP with the `steal` feature.
#[derive(Debug)]
pub struct StealHttpSettings {
    /// The HTTP filter to use.
    pub filter: StealHttpFilter,
    /// Ports to filter HTTP on.
    pub ports: HashSet<Port>,
}

/// Operation mode for the `incoming` feature.
#[derive(Debug)]
pub enum IncomingMode {
    /// The agent sends data to both the user application and the remote target.
    /// Data coming from the layer is discarded.
    Mirror,
    /// The agent sends data only to the user application.
    /// Data coming from the layer is sent to the agent.
    Steal(StealHttpSettings),
}

impl IncomingMode {
    /// Creates a new instance from the given [`LayerConfig`].
    fn new(config: &IncomingConfig) -> Self {
        if !config.is_steal() {
            return Self::Mirror;
        }

        let http_filter_config = &config.http_filter;

        let ports = {
            http_filter_config
                .ports
                .as_slice()
                .iter()
                .copied()
                .collect()
        };

        let filter = match (
            &http_filter_config.path_filter,
            &http_filter_config.header_filter,
        ) {
            (Some(path), None) => StealHttpFilter::Filter(HttpFilter::Path(
                Filter::new(path.into()).expect("invalid filter expression"),
            )),
            (None, Some(header)) => StealHttpFilter::Filter(HttpFilter::Header(
                Filter::new(header.into()).expect("invalid filter expression"),
            )),
            (None, None) => StealHttpFilter::None,
            _ => panic!("multiple HTTP filters specified"),
        };

        Self::Steal(StealHttpSettings { filter, ports })
    }

    /// Returns [`PortSubscription`] request to be used for the given port.
    pub fn subscription(&self, port: Port) -> PortSubscription {
        let Self::Steal(steal) = self else {
            return PortSubscription::Mirror(port);
        };

        let steal_type = match &steal.filter {
            _ if !steal.ports.contains(&port) => StealType::All(port),
            StealHttpFilter::None => StealType::All(port),
            StealHttpFilter::Filter(filter) => StealType::FilteredHttpEx(port, filter.clone()),
        };

        PortSubscription::Steal(steal_type)
    }
}
