use std::{collections::HashSet, net::SocketAddr};

use mirrord_config::{
    feature::{
        fs::FsConfig,
        network::{incoming::IncomingConfig, outgoing::OutgoingConfig},
    },
    util::VecOrSingle,
    LayerConfig,
};
use mirrord_intproxy::protocol::PortSubscription;
use mirrord_protocol::{
    tcp::{Filter, HttpFilter, StealType},
    Port,
};
use regex::RegexSet;

use crate::{
    debugger_ports::DebuggerPorts,
    detour::{Bypass, Detour},
    file::filter::FileFilter,
    socket::OutgoingSelector,
};

#[derive(Debug)]
pub struct LayerSetup {
    config: LayerConfig,
    file_filter: FileFilter,
    debugger_ports: DebuggerPorts,
    remote_unix_streams: RegexSet,
    outgoing_selector: OutgoingSelector,
    proxy_address: SocketAddr,
    incoming_mode: IncomingMode,
}

impl LayerSetup {
    pub fn new(config: LayerConfig, debugger_ports: DebuggerPorts) -> Self {
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

        Self {
            config,
            file_filter,
            debugger_ports,
            remote_unix_streams,
            outgoing_selector,
            proxy_address,
            incoming_mode,
        }
    }

    pub fn fs_config(&self) -> &FsConfig {
        &self.config.feature.fs
    }

    pub fn incoming_config(&self) -> &IncomingConfig {
        &self.config.feature.network.incoming
    }

    pub fn outgoing_config(&self) -> &OutgoingConfig {
        &self.config.feature.network.outgoing
    }

    pub fn remote_dns_enabled(&self) -> bool {
        self.config.feature.network.dns
    }

    pub fn targetless(&self) -> bool {
        self.config.target.path.is_none()
    }

    pub fn filter_ignored_file(&self, path: &str, write: bool) -> Detour<()> {
        self.file_filter
            .continue_or_bypass_with(path, write, || Bypass::IgnoredFile(path.into()))
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
}

/// HTTP filter used by the layer with the `steal` feature.
#[derive(Debug)]
pub enum StealHttpFilter {
    /// No filter.
    None,
    /// Header filter, deprecated.
    HeaderDeprecated(Filter),
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

/// Operation mode for the [`IncomingProxy`](super::IncomingProxy).
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

        let http_header_filter_config = &config.http_header_filter;
        let http_filter_config = &config.http_filter;

        let ports = {
            if http_header_filter_config.filter.is_some() {
                http_header_filter_config
                    .ports
                    .as_slice()
                    .iter()
                    .copied()
                    .collect()
            } else {
                http_filter_config
                    .ports
                    .as_slice()
                    .iter()
                    .copied()
                    .collect()
            }
        };

        let filter = match (
            &http_filter_config.path_filter,
            &http_filter_config.header_filter,
            &http_header_filter_config.filter,
        ) {
            (Some(path), None, None) => StealHttpFilter::Filter(HttpFilter::Path(
                Filter::new(path.into()).expect("invalid filter expression"),
            )),
            (None, Some(header), None) => StealHttpFilter::Filter(HttpFilter::Header(
                Filter::new(header.into()).expect("invalid filter expression"),
            )),
            (None, None, Some(header)) => StealHttpFilter::HeaderDeprecated(
                Filter::new(header.into()).expect("invalid filter expression"),
            ),
            (None, None, None) => StealHttpFilter::None,
            _ => panic!("multiple HTTP filters specified"),
        };

        Self::Steal(StealHttpSettings { filter, ports })
    }

    /// Returns [`PortSubscription`] to be used with the given port.
    pub fn subscription(&self, port: Port) -> PortSubscription {
        let Self::Steal(steal) = self else {
            return PortSubscription::Mirror(port);
        };

        let steal_type = match &steal.filter {
            _ if !steal.ports.contains(&port) => StealType::All(port),
            StealHttpFilter::None => StealType::All(port),
            StealHttpFilter::HeaderDeprecated(filter) => {
                StealType::FilteredHttp(port, filter.clone())
            }
            StealHttpFilter::Filter(filter) => StealType::FilteredHttpEx(port, filter.clone()),
        };

        PortSubscription::Steal(steal_type)
    }
}
