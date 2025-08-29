use std::{collections::HashSet, net::SocketAddr, ops::Deref, str::FromStr};

use mirrord_config::feature::network::{
    dns::{DnsConfig, DnsFilterConfig}, 
    outgoing::{OutgoingConfig, OutgoingFilterConfig}, 
    filter::{AddressFilter, ProtocolAndAddressFilter, ProtocolFilter},
};
use mirrord_intproxy_protocol::NetProtocol;

/// Common outgoing connection selector that can be used by both Windows and Unix layers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum OutgoingSelector {
    #[default]
    Unfiltered,
    /// If the address from `connect` matches this, then we send the connection through the
    /// remote pod.
    Remote(HashSet<ProtocolAndAddressFilter>),
    /// If the address from `connect` matches this, then we send the connection from the local app.
    Local(HashSet<ProtocolAndAddressFilter>),
}

impl OutgoingSelector {
    fn build_selector<'a, I: Iterator<Item = &'a str>>(
        filters: I,
        tcp_enabled: bool,
        udp_enabled: bool,
    ) -> HashSet<ProtocolAndAddressFilter> {
        filters
            .map(|filter| {
                ProtocolAndAddressFilter::from_str(filter).expect("invalid outgoing filter")
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|ProtocolAndAddressFilter { protocol, .. }| match protocol {
                ProtocolFilter::Any => tcp_enabled || udp_enabled,
                ProtocolFilter::Tcp => tcp_enabled,
                ProtocolFilter::Udp => udp_enabled,
            })
            .collect::<HashSet<_>>()
    }

    /// Builds a new instance from the user config, removing filters
    /// that would create inconsistencies, by checking if their protocol is enabled for outgoing
    /// traffic, and thus we avoid making this check on every connect call.
    ///
    /// It also removes duplicated filters, by putting them into a HashSet.
    pub fn new(config: &OutgoingConfig) -> Self {
        match &config.filter {
            None => Self::Unfiltered,
            Some(OutgoingFilterConfig::Remote(list)) | Some(OutgoingFilterConfig::Local(list))
                if list.is_empty() =>
            {
                panic!("outgoing traffic filter cannot be empty");
            }
            Some(OutgoingFilterConfig::Remote(list)) => Self::Remote(Self::build_selector(
                list.iter().map(String::as_str),
                config.tcp,
                config.udp,
            )),
            Some(OutgoingFilterConfig::Local(list)) => Self::Local(Self::build_selector(
                list.iter().map(String::as_str),
                config.tcp,
                config.udp,
            )),
        }
    }

    /// Returns whether this selector is configured for remote traffic
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    /// Returns whether this selector is configured for local traffic
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }

    /// Returns whether this selector is unfiltered
    pub fn is_unfiltered(&self) -> bool {
        matches!(self, Self::Unfiltered)
    }

    /// Gets the filters for the selector, if any
    pub fn filters(&self) -> Option<&HashSet<ProtocolAndAddressFilter>> {
        match self {
            Self::Remote(filters) | Self::Local(filters) => Some(filters),
            Self::Unfiltered => None,
        }
    }

    /// Check if a connection should go through remote or local based on filters (Windows layer compatibility)
    pub fn get_connection_through(&self, address: SocketAddr, protocol: NetProtocol) -> Result<ConnectionThrough, Box<dyn std::error::Error>> {
        // For now, we'll use simplified logic since we don't have all the Unix layer infrastructure
        let result = match self {
            Self::Unfiltered => ConnectionThrough::Remote(address),
            Self::Remote(filters) => {
                // Check if any filter matches
                for filter in filters {
                    if self.filter_matches(filter, address, protocol) {
                        return Ok(ConnectionThrough::Remote(address));
                    }
                }
                ConnectionThrough::Local(address)
            }
            Self::Local(filters) => {
                // Check if any filter matches
                for filter in filters {
                    if self.filter_matches(filter, address, protocol) {
                        return Ok(ConnectionThrough::Local(address));
                    }
                }
                ConnectionThrough::Remote(address)
            }
        };
        Ok(result)
    }

    /// Helper function to check if a filter matches the given address and protocol
    fn filter_matches(&self, filter: &ProtocolAndAddressFilter, address: SocketAddr, protocol: NetProtocol) -> bool {
        // Check protocol match
        let protocol_matches = match (&filter.protocol, protocol) {
            (ProtocolFilter::Any, _) => true,
            (ProtocolFilter::Tcp, NetProtocol::Stream) => true,
            (ProtocolFilter::Udp, NetProtocol::Datagrams) => true,
            _ => false,
        };

        if !protocol_matches {
            return false;
        }

        // Check address match
        match &filter.address {
            AddressFilter::Port(port) => *port == 0 || *port == address.port(),
            AddressFilter::Socket(socket_addr) => {
                (socket_addr.ip().is_unspecified() || socket_addr.ip() == address.ip()) &&
                (socket_addr.port() == 0 || socket_addr.port() == address.port())
            },
            AddressFilter::Name(_name, port) => {
                // For hostname filtering, we would need DNS resolution
                // This is simplified for now
                *port == 0 || *port == address.port()
            },
            AddressFilter::Subnet(subnet, port) => {
                subnet.contains(&address.ip()) && 
                (*port == 0 || *port == address.port())
            },
        }
    }
}

/// Result of outgoing connection filtering
#[derive(Debug, Clone)]
pub enum ConnectionThrough {
    /// Connection should go through the local app
    Local(SocketAddr),
    /// Connection should go through the remote pod  
    Remote(SocketAddr),
}

/// Common functionality for filtering outgoing connections
pub trait OutgoingFilter {
    /// Checks if the address matches the outgoing filter and returns how the connection should be routed
    fn get_connection_through(&self, address: SocketAddr, protocol: NetProtocol) -> Result<ConnectionThrough, Box<dyn std::error::Error>>;
}
