use std::net::SocketAddr;

use mirrord_config::{
    feature::{
        fs::FsConfig,
        network::{incoming::IncomingConfig, outgoing::OutgoingConfig},
    },
    util::VecOrSingle,
    LayerConfig,
};
use regex::RegexSet;

use crate::{
    debugger_ports::DebuggerPorts,
    detour::{Bypass, Detour},
    file::filter::FileFilter,
    socket::OutgoingSelector,
};

/// Complete layer state that can be safely used in multi-threaded applications and single-threaded
/// applications using forks.
#[derive(Debug)]
pub struct LayerState {
    config: LayerConfig,
    file_filter: FileFilter,
    debugger_ports: DebuggerPorts,
    remote_unix_streams: RegexSet,
    outgoing_selector: OutgoingSelector,
    proxy_address: SocketAddr,
}

impl LayerState {
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

        Self {
            config,
            file_filter,
            debugger_ports,
            remote_unix_streams,
            outgoing_selector,
            proxy_address,
        }
    }

    pub fn file_filter(&self) -> &FileFilter {
        &self.file_filter
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

    #[clippy::must_use]
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
}
