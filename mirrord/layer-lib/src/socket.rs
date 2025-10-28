pub mod dns;
pub mod hostname;
pub mod ops;
pub mod sockets;

use std::{collections::HashSet, net::SocketAddr, ops::Deref, str::FromStr};

use bincode::{Decode, Encode};
// Re-export dns module items
pub use dns::{
    REMOTE_DNS_REVERSE_MAPPING, clear_dns_reverse_mapping, dns_reverse_mapping_size,
    get_hostname_for_ip, update_dns_reverse_mapping,
};
// Cross-platform socket constants
#[cfg(unix)]
use libc::{AF_INET, AF_INET6, SOCK_DGRAM, SOCK_STREAM};
use mirrord_config::feature::network::{
    dns::{DnsConfig, DnsFilterConfig},
    filter::{AddressFilter, ProtocolAndAddressFilter, ProtocolFilter},
    outgoing::{OutgoingConfig, OutgoingFilterConfig},
};
use mirrord_intproxy_protocol::{NetProtocol, PortUnsubscribe};
use mirrord_protocol::outgoing::SocketAddress;
// Re-export ops module items
pub use ops::{
    ConnectFn, ConnectResult, SendtoFn, connect_outgoing, connect_outgoing_udp,
    create_outgoing_request, is_unix_address, prepare_outgoing_address, send_dns_patch, send_to,
    update_socket_connected_state,
};
use socket2::SockAddr;
// Re-export sockets module items
pub use sockets::{
    SHARED_SOCKETS_ENV_VAR, SOCKETS, SocketDescriptor, get_bound_address, get_connected_addresses,
    get_socket, get_socket_state, is_socket_in_state, is_socket_managed, register_socket,
    remove_socket, set_socket_state, shared_sockets,
};
#[cfg(windows)]
use winapi::shared::ws2def::{AF_INET, AF_INET6, SOCK_DGRAM, SOCK_STREAM};

pub use crate::{ConnectError, HookResult, proxy_connection::make_proxy_request_no_response};

/// Contains the addresses of a mirrord connected socket.
///
/// - `layer_address` is only used for the outgoing feature.
#[derive(Debug, Clone, Encode, Decode)]
pub struct Connected {
    /// The address requested by the user that we're "connected" to.
    ///
    /// Whenever the user calls `getpeername`, this is the address we return to them.
    ///
    /// For the _outgoing_ feature, we actually connect to the `layer_address` interceptor socket,
    /// but use this address in the `recvfrom` handling of `fill_address`.
    pub remote_address: SocketAddress,

    /// Local address (pod-wise)
    ///
    /// ## Example
    ///
    /// ```sh
    /// $ kubectl get pod -o wide
    ///
    /// NAME             READY   STATUS    IP
    /// impersonated-pod 0/1     Running   1.2.3.4
    /// ```
    ///
    /// We would set this ip as `1.2.3.4:{port}` in `bind`, where `{port}` is the user requested
    /// port.
    pub local_address: SocketAddress,

    /// The address of the interceptor socket, this is what we're really connected to in the
    /// outgoing feature.
    pub layer_address: Option<SocketAddress>,
}

/// Represents a [`SocketState`] where the user made a `bind` call, and we intercepted it.
///
/// ## Details
///
/// Our `bind` hook doesn't bind the address that the user passed to us, instead we call
/// the OS bind function with `localhost:0` (or `unspecified:0` for ipv6), and use
/// `getsockname` to retrieve this bound address which we assign to `Bound::address`.
///
/// The original user requested address is assigned to `Bound::requested_address`, and used as an
/// illusion for when the user calls `getsockname`, as if this address was the actual local
/// bound address.
#[derive(Debug, Clone, Copy, Encode, Decode)]
pub struct Bound {
    /// Address originally requested by the user for `bind`.
    pub requested_address: SocketAddr,

    /// Actual bound address that we use to communicate between the user's listener socket and our
    /// interceptor socket.
    pub address: SocketAddr,
}

#[derive(Debug, Default, Clone, Encode, Decode)]
pub enum SocketState {
    #[default]
    Initialized,
    Bound(Bound),
    Listening(Bound),
    Connected(Connected),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum SocketKind {
    Tcp(i32),
    Udp(i32),
}

impl SocketKind {
    pub const fn is_udp(self) -> bool {
        matches!(self, Self::Udp(..))
    }

    pub const fn is_tcp(self) -> bool {
        matches!(self, Self::Tcp(..))
    }
}

impl TryFrom<i32> for SocketKind {
    type Error = i32;

    fn try_from(socket_type: i32) -> Result<Self, Self::Error> {
        // Mask out any flags (like SOCK_NONBLOCK, SOCK_CLOEXEC) to get the base socket type
        let base_type = socket_type & 0xFF;

        match base_type {
            SOCK_STREAM => Ok(SocketKind::Tcp(socket_type)),
            SOCK_DGRAM => Ok(SocketKind::Udp(socket_type)),
            _ => {
                // Return error for unknown socket types instead of defaulting
                Err(socket_type)
            }
        }
    }
}

impl From<SocketKind> for NetProtocol {
    fn from(kind: SocketKind) -> Self {
        match kind {
            SocketKind::Tcp(..) => Self::Stream,
            SocketKind::Udp(..) => Self::Datagrams,
        }
    }
}

// TODO(alex): We could treat `sockfd` as being the same as `&self` for socket ops, we currently
// can't do that due to how `dup` interacts directly with our `Arc<UserSocket>`, because we just
// `clone` the arc, we end up with exact duplicates, but `dup` generates a new fd that we have no
// way of putting inside the duplicated `UserSocket`.
/// Cross-platform socket structure that holds socket metadata and state.
#[derive(Debug, Clone, Encode, Decode)]
pub struct UserSocket {
    pub domain: i32,
    pub type_: i32,
    pub protocol: i32,
    pub state: SocketState,
    pub kind: SocketKind,
}

impl UserSocket {
    pub fn new(
        domain: i32,
        type_: i32,
        protocol: i32,
        state: SocketState,
        kind: SocketKind,
    ) -> Self {
        Self {
            domain,
            type_,
            protocol,
            state,
            kind,
        }
    }

    /// Gets the bound address from the socket state, if available
    pub fn bound_address(&self) -> Option<Bound> {
        match &self.state {
            SocketState::Bound(bound) | SocketState::Listening(bound) => Some(*bound),
            _ => None,
        }
    }

    /// Gets the connected address from the socket state, if available
    pub fn connected_address(&self) -> Option<&Connected> {
        match &self.state {
            SocketState::Connected(connected) => Some(connected),
            _ => None,
        }
    }

    /// Checks if the socket is in listening state
    pub fn is_listening(&self) -> bool {
        matches!(self.state, SocketState::Listening(_))
    }

    /// Checks if the socket is connected
    pub fn is_connected(&self) -> bool {
        matches!(self.state, SocketState::Connected(_))
    }

    /// Checks if the socket is bound
    pub fn is_bound(&self) -> bool {
        matches!(
            self.state,
            SocketState::Bound(_) | SocketState::Listening(_)
        )
    }

    /// Closes the socket and performs necessary cleanup.
    /// If this socket was listening and bound to a port, notifies agent to stop
    /// mirroring/stealing that port by sending PortUnsubscribe.
    pub fn close(&self) {
        if self.is_listening()
            && let Some(bound) = self.bound_address()
        {
            let port_unsubscribe = PortUnsubscribe {
                port: bound.address.port(),
                listening_on: bound.address,
            };

            // Send unsubscribe request to stop port operations
            let _ = make_proxy_request_no_response(port_unsubscribe);

            // For steal mode on Windows, add a small delay to allow agent processing
            // This prevents race conditions where subsequent requests arrive before
            // the agent has processed the port unsubscription
            #[cfg(target_os = "windows")]
            {
                use crate::setup::layer_setup;

                if layer_setup().incoming_mode().steal {
                    // Small delay to ensure agent processes unsubscription
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
}

/// Trait for DNS resolution functionality that can be implemented by platform-specific layers
pub trait DnsResolver {
    type Error;

    /// Resolve a hostname to IP addresses
    fn resolve_hostname(
        hostname: &str,
        port: u16,
        family: i32,
        protocol: i32,
    ) -> Result<Vec<std::net::IpAddr>, Self::Error>;

    /// Check if remote DNS is enabled
    fn remote_dns_enabled() -> bool;
}

/// Cross-platform address conversion utilities
pub trait SocketAddrExt {
    // Platform-specific implementations should provide their own address conversion methods
}

impl SocketAddrExt for SockAddr {
    // Platform-specific implementations should override this
}

/// Helper function to check if a port should be ignored (port 0)
#[inline]
pub fn is_ignored_port(addr: &SocketAddr) -> bool {
    addr.port() == 0
}

/// Holds valid address that we should use to `connect_outgoing`.
#[derive(Debug, Clone, Copy)]
pub enum ConnectionThrough {
    /// Connect locally, this means just call `FN_CONNECT` on the inner [`SocketAddr`].
    Local(SocketAddr),

    /// Connect through the agent.
    Remote(SocketAddr),
}

/// Holds the [`ProtocolAndAddressFilter`]s set up by the user in the [`OutgoingFilterConfig`].
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
    /// that would create inconsitencies, by checking if their protocol is enabled for outgoing
    /// traffic, and thus we avoid making this check on every `connect` call.
    ///
    /// It also removes duplicated filters, by putting them into a [`HashSet`].
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

    /// Checks if the `address` matches the specified outgoing filter.
    ///
    /// Returns either a [`ConnectionThrough::Remote`] or a [`ConnectionThrough::Local`], with the
    /// address that the user application should be connected to.
    ///
    /// ## `remote`
    ///
    /// When the user specifies something like `remote = [":7777"]`, we're going to check if
    /// the `address` has `port == 7777`. The same idea can be extrapolated to the other accepted
    /// values for this config, such as subnet, hostname, ip (and combinations of those).
    ///
    /// ## `local`
    ///
    /// Basically the same thing as `remote`, but the result is reversed, meaning that, if
    /// `address` matches something specified in `local = [":7777"]`, then we return a
    /// [`ConnectionThrough::Local`].
    ///
    /// ## Filter rules
    ///
    /// The filter comparison follows these rules:
    ///
    /// 1. `0.0.0.0` means any ip;
    /// 2. `:0` means any port;
    ///
    /// So if the user specified a selector with `0.0.0.0:0`, we're going to be always matching on
    /// it.
    pub fn get_connection_through_with_resolver<R: DnsResolver>(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
    ) -> Result<ConnectionThrough, R::Error> {
        let (filters, selector_is_local) = match self {
            Self::Unfiltered => return Ok(ConnectionThrough::Remote(address)),
            Self::Local(filters) => (filters, true),
            Self::Remote(filters) => (filters, false),
        };

        for filter in filters {
            if !filter.matches_with_resolver::<R>(address, protocol, selector_is_local)? {
                continue;
            }

            return if selector_is_local {
                // For local connections, platform-specific implementations should handle address
                // resolution
                Ok(ConnectionThrough::Local(address))
            } else {
                Ok(ConnectionThrough::Remote(address))
            };
        }

        if selector_is_local {
            Ok(ConnectionThrough::Remote(address))
        } else {
            // For local fallback, platform-specific implementations should handle address
            // resolution
            Ok(ConnectionThrough::Local(address))
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
}

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

/// [`ProtocolAndAddressFilter`] extension.
/// Advanced filter matching with DNS resolution capability
pub trait ProtocolAndAddressFilterExt {
    /// Matches the outgoing connection request (given as [[`SocketAddr`], [`NetProtocol`]] pair)
    /// against this filter.
    ///
    /// # Note on DNS resolution
    ///
    /// This method may require a DNS resolution (when [`ProtocolAndAddressFilter::address`] is
    /// [`AddressFilter::Name`]). If remote DNS is disabled or `force_local_dns`
    /// flag is used, the method uses local resolution `ToSocketAddrs`. Otherwise, it uses
    /// remote resolution `remote_getaddrinfo`.
    /// Matches the outgoing connection request against this filter with optional DNS resolution
    fn matches_with_resolver<R: DnsResolver>(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
        force_local_dns: bool,
    ) -> Result<bool, R::Error>;
}

impl ProtocolAndAddressFilterExt for ProtocolAndAddressFilter {
    fn matches_with_resolver<R: DnsResolver>(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
        force_local_dns: bool,
    ) -> Result<bool, R::Error> {
        // Check protocol match
        let protocol_matches = matches!(
            (&self.protocol, protocol),
            (ProtocolFilter::Any, _)
                | (ProtocolFilter::Tcp, NetProtocol::Stream)
                | (ProtocolFilter::Udp, NetProtocol::Datagrams)
        );
        if !protocol_matches {
            return Ok(false);
        }

        let port = self.address.port();
        if port != 0 && port != address.port() {
            return Ok(false);
        }

        let family = if address.is_ipv4() { AF_INET } else { AF_INET6 };

        let addr_protocol = if matches!(protocol, NetProtocol::Stream) {
            SOCK_STREAM
        } else {
            SOCK_DGRAM
        };

        match &self.address {
            AddressFilter::Name(name, port) => {
                let resolved_ips = if R::remote_dns_enabled() && !force_local_dns {
                    R::resolve_hostname(name, *port, family, addr_protocol)?
                } else {
                    // Use standard library DNS resolution as fallback
                    use std::net::ToSocketAddrs;
                    match (name.as_str(), *port).to_socket_addrs() {
                        Ok(addresses) => addresses.map(|addr| addr.ip()).collect(),
                        Err(_) => vec![], // No records found
                    }
                };

                Ok(resolved_ips.into_iter().any(|ip| ip == address.ip()))
            }
            AddressFilter::Socket(addr) => {
                Ok(addr.ip().is_unspecified() || addr.ip() == address.ip())
            }
            AddressFilter::Subnet(net, _) => Ok(net.contains(&address.ip())),
            AddressFilter::Port(..) => Ok(true),
        }
    }
}
