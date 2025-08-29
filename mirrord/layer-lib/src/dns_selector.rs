use std::ops::Deref;

use mirrord_config::feature::network::{
    dns::{DnsConfig, DnsFilterConfig},
    filter::AddressFilter,
};

/// Generated from [`DnsConfig`] provided in the [`LayerConfig`](mirrord_config::LayerConfig).
/// Decides whether DNS queries are done locally or remotely.
#[derive(Debug)]
pub struct DnsSelector {
    /// Filters provided in the config.
    filters: Vec<AddressFilter>,
    /// Whether a query matching one of [`Self::filters`] should be done locally.
    filter_is_local: bool,
}

impl DnsSelector {
    /// Creates a new DnsSelector from configuration
    pub fn new(config: &DnsConfig) -> Self {
        if !config.enabled {
            return Self {
                filters: Default::default(),
                filter_is_local: false,
            };
        }

        let (filters, filter_is_local) = match &config.filter {
            Some(DnsFilterConfig::Local(filters)) => (Some(filters.deref()), true),
            Some(DnsFilterConfig::Remote(filters)) => (Some(filters.deref()), false),
            None => (None, true),
        };

        let filters = filters
            .into_iter()
            .flatten()
            .map(|filter| {
                filter
                    .parse::<AddressFilter>()
                    .expect("bad address filter, should be verified in the CLI")
            })
            .collect();

        Self {
            filters,
            filter_is_local,
        }
    }

    /// Returns whether DNS is enabled (based on whether filters exist)
    pub fn is_enabled(&self) -> bool {
        !self.filters.is_empty() || self.filter_is_local
    }

    /// Returns whether queries matching filters should be done locally
    pub fn filter_is_local(&self) -> bool {
        self.filter_is_local
    }

    /// Gets the filters
    pub fn filters(&self) -> &[AddressFilter] {
        &self.filters
    }

    /// Checks if a query should be handled locally or remotely.
    /// Returns true if it should be handled locally.
    pub fn should_be_local(&self, node: &str, port: u16) -> bool {
        let matched = self
            .filters
            .iter()
            .filter(|filter| {
                let filter_port = filter.port();
                filter_port == 0 || filter_port == port
            })
            .any(|filter| match filter {
                AddressFilter::Port(..) => true,
                AddressFilter::Name(filter_name, _) => filter_name == node,
                AddressFilter::Socket(filter_socket) => {
                    filter_socket.ip().is_unspecified()
                        || Some(filter_socket.ip()) == node.parse().ok()
                }
                AddressFilter::Subnet(filter_subnet, _) => {
                    let Ok(ip): std::result::Result<std::net::IpAddr, _> = node.parse() else {
                        return false;
                    };

                    filter_subnet.contains(&ip)
                }
            });

        matched == self.filter_is_local
    }

    /// Check if a DNS query should be resolved remotely (Windows layer compatibility)
    pub fn should_resolve_remotely(&self, node: &str, port: u16) -> bool {
        !self.should_be_local(node, port)
    }

    /// Bypasses queries that should be done locally (Unix layer compatibility)
    /// This returns a platform-specific result type that can be used by the Unix layer
    pub fn check_query_result(&self, node: &str, port: u16) -> CheckQueryResult {
        if self.should_be_local(node, port) {
            CheckQueryResult::Local
        } else {
            CheckQueryResult::Remote
        }
    }
}

impl From<&DnsConfig> for DnsSelector {
    fn from(config: &DnsConfig) -> Self {
        Self::new(config)
    }
}

/// Result of DNS query checking - whether to handle locally or remotely
#[derive(Debug, PartialEq, Eq)]
pub enum CheckQueryResult {
    /// Handle the query locally
    Local,
    /// Handle the query remotely
    Remote,
}
