#[cfg(target_os = "macos")]
pub mod apple_dnsinfo;
pub mod dns;
pub mod dns_selector;
pub mod hostname;
pub mod ops;
pub mod sockets;

#[cfg(windows)]
use std::mem;
use std::{
    collections::HashSet,
    net::{SocketAddr, ToSocketAddrs},
    str::FromStr,
};

use bincode::{Decode, Encode};
// Re-export dns module items
pub use dns::reverse_dns::get_hostname_for_ip;
use libc::c_int;
// Cross-platform socket constants
#[cfg(unix)]
pub use libc::{AF_INET, AF_INET6, AF_UNIX, SOCK_DGRAM, SOCK_STREAM, sockaddr, socklen_t};
use mirrord_config::feature::network::{
    filter::{AddressFilter, ProtocolAndAddressFilter, ProtocolFilter},
    outgoing::{OutgoingConfig, OutgoingFilterConfig},
};
use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnCloseRequest, PortUnsubscribe};
use mirrord_protocol::{
    DnsLookupError, ResolveErrorKindInternal, ResponseError, outgoing::SocketAddress,
};
#[cfg(unix)]
use socket2::SockAddr;
// Re-export sockets module items
pub use sockets::{
    SHARED_SOCKETS_ENV_VAR, SOCKETS, SocketDescriptor, get_bound_address, get_connected_addresses,
    get_socket, get_socket_state, is_socket_in_state, is_socket_managed, register_socket,
    remove_socket,
};
#[cfg(windows)]
pub use winapi::{
    shared::{
        ws2def::{
            AF_INET, AF_INET6, AF_UNIX, SOCK_DGRAM, SOCK_STREAM, SOCKADDR, SOCKADDR_IN,
            SOCKADDR_STORAGE,
        },
        ws2ipdef::SOCKADDR_IN6,
    },
    um::{winnt::INT, winsock2::WSAEFAULT},
};

#[cfg(unix)]
use crate::detour::DetourGuard;
#[cfg(windows)]
use crate::error::windows::{WindowsError, WindowsResult};
pub use crate::{
    ConnectError, HookError, HookResult, detour::Bypass,
    proxy_connection::make_proxy_request_no_response, setup::setup,
    socket::dns::remote_getaddrinfo,
};

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

    /// Local address of the agent's socket.
    ///
    /// Whenever the user calls `getsockname`, this is the address we return to them.
    ///
    /// Not available in case of experimental non-blocking TCP connections.
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
    pub local_address: Option<SocketAddress>,

    /// The address of the interceptor socket, this is what we're really connected to in the
    /// outgoing feature.
    pub layer_address: Option<SocketAddress>,

    /// Unique ID of this connection.
    ///
    /// Only for sockets used for outgoing connections.
    pub connection_id: Option<u128>,
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
    Bound {
        bound: Bound,
        // if true, the socket has a mapped BUT should not be used to make port subscriptions
        // used for listen_ports (COR-1014).
        is_only_bound: bool,
    },
    Listening(Bound),
    Connected(Connected),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum SocketKind {
    Tcp(c_int),
    Udp(c_int),
}

impl SocketKind {
    pub const fn is_udp(self) -> bool {
        matches!(self, Self::Udp(..))
    }

    pub const fn is_tcp(self) -> bool {
        matches!(self, Self::Tcp(..))
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

impl TryFrom<c_int> for SocketKind {
    #[cfg(unix)]
    type Error = Bypass;
    #[cfg(windows)]
    type Error = i32;

    fn try_from(type_: c_int) -> Result<Self, Self::Error> {
        if (type_ & SOCK_STREAM) > 0 {
            Ok(SocketKind::Tcp(type_))
        } else if (type_ & SOCK_DGRAM) > 0 {
            Ok(SocketKind::Udp(type_))
        } else {
            #[cfg(unix)]
            return Err(Bypass::Type(type_));
            #[cfg(windows)]
            return Err(type_);
        }
    }
}

// TODO(alex): We could treat `sockfd` as being the same as `&self` for socket ops, we currently
// can't do that due to how `dup` interacts directly with our `Arc<UserSocket>`, because we just
// `clone` the arc, we end up with exact duplicates, but `dup` generates a new fd that we have no
// way of putting inside the duplicated `UserSocket`.
#[derive(Debug, Encode, Decode, Clone)]
pub struct UserSocket {
    pub domain: c_int,
    pub type_: c_int,
    pub protocol: c_int,
    pub state: SocketState,
    pub kind: SocketKind,
}

impl UserSocket {
    pub fn new(
        domain: c_int,
        type_: c_int,
        protocol: c_int,
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

    /// Closes the socket and performs necessary cleanup.
    /// If this socket was listening and bound to a port, notifies agent to stop
    /// mirroring/stealing that port by sending PortUnsubscribe.
    ///
    /// **Important**
    ///
    /// Before calling this method, make sure that this socket does not have any living clones
    /// (dup*) in the current process.
    pub fn close(&self) {
        match self {
            Self {
                state: SocketState::Listening(bound),
                kind: SocketKind::Tcp(..),
                ..
            } => {
                let _ = make_proxy_request_no_response(PortUnsubscribe {
                    port: bound.requested_address.port(),
                    listening_on: bound.address,
                });
            }
            Self {
                state:
                    SocketState::Connected(Connected {
                        connection_id: Some(id),
                        ..
                    }),
                ..
            } => {
                let _ = make_proxy_request_no_response(OutgoingConnCloseRequest { conn_id: *id });
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
pub trait SocketAddrExt {
    /// Converts a raw [`sockaddr`] pointer into a more _Rusty_ type
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> HookResult<Self>
    where
        Self: Sized;
}

#[cfg(unix)]
impl SocketAddrExt for SockAddr {
    fn try_from_raw(
        raw_address: *const sockaddr,
        address_length: socklen_t,
    ) -> HookResult<SockAddr> {
        unsafe {
            SockAddr::try_init(|storage, len| {
                // storage and raw_address size is dynamic.
                (storage as *mut u8)
                    .copy_from_nonoverlapping(raw_address as *const u8, address_length as usize);
                *len = address_length;
                Ok(())
            })
        }
        .map_err(|_| HookError::Bypass(Bypass::AddressConversion))
        .map(|((), address)| address)
    }
}

#[cfg(unix)]
impl SocketAddrExt for SocketAddr {
    fn try_from_raw(
        raw_address: *const sockaddr,
        address_length: socklen_t,
    ) -> HookResult<SocketAddr> {
        SockAddr::try_from_raw(raw_address, address_length).and_then(|address| {
            address
                .as_socket()
                .ok_or(HookError::Bypass(Bypass::AddressConversion))
        })
    }
}

#[cfg(windows)]
pub trait SocketAddrExt {
    /// Converts a raw Windows SOCKADDR pointer into a more _Rusty_ type.
    ///
    /// # Safety
    /// The caller must guarantee that `raw_address` points to readable memory for at least
    /// `address_length` bytes containing a valid `SOCKADDR` (or larger) structure originating from
    /// the OS. Passing dangling or undersized pointers causes undefined behavior.
    unsafe fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<Self>
    where
        Self: Sized;

    /// Copies the socket address data into a Windows SOCKADDR structure for API calls.
    ///
    /// # Safety
    /// `name` must point to writable memory of size `*namelen`, and `namelen` must be a valid
    /// pointer to an `INT`. The buffer must be large enough for the address family stored in
    /// `self`.
    unsafe fn copy_to(&self, name: *mut SOCKADDR, namelen: *mut INT) -> WindowsResult<()>;

    /// Creates an owned SOCKADDR representation of this address
    fn to_sockaddr(&self) -> WindowsResult<(SOCKADDR_STORAGE, INT)> {
        let mut storage: SOCKADDR_STORAGE = unsafe { mem::zeroed() };
        let mut len = mem::size_of::<SOCKADDR_STORAGE>() as INT;
        unsafe {
            self.copy_to(&mut storage as *mut _ as *mut SOCKADDR, &mut len)?;
        }
        Ok((storage, len))
    }
}

#[cfg(windows)]
impl SocketAddrExt for SocketAddr {
    unsafe fn try_from_raw(
        raw_address: *const SOCKADDR,
        address_length: INT,
    ) -> Option<SocketAddr> {
        unsafe { sockaddr_to_socket_addr(raw_address, address_length) }
    }
    unsafe fn copy_to(&self, name: *mut SOCKADDR, namelen: *mut INT) -> WindowsResult<()> {
        unsafe { socketaddr_to_windows_sockaddr(self, name, namelen) }
    }
}

#[cfg(windows)]
impl SocketAddrExt for SocketAddress {
    unsafe fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<Self> {
        unsafe { SocketAddr::try_from_raw(raw_address, address_length) }.map(SocketAddress::from)
    }
    unsafe fn copy_to(&self, name: *mut SOCKADDR, namelen: *mut INT) -> WindowsResult<()> {
        let std_addr =
            SocketAddr::try_from(self.clone()).map_err(|_| WindowsError::WinSock(WSAEFAULT))?;
        unsafe { std_addr.copy_to(name, namelen) }
    }
    fn to_sockaddr(&self) -> WindowsResult<(SOCKADDR_STORAGE, INT)> {
        let std_addr =
            SocketAddr::try_from(self.clone()).map_err(|_| WindowsError::WinSock(WSAEFAULT))?;
        std_addr.to_sockaddr()
    }
}
/// Helper function to convert Windows SOCKADDR to Rust SocketAddr
#[cfg(windows)]
unsafe fn sockaddr_to_socket_addr(addr: *const SOCKADDR, addrlen: INT) -> Option<SocketAddr> {
    if addr.is_null() || addrlen < mem::size_of::<SOCKADDR_IN>() as INT {
        return None;
    }

    let sa_family = unsafe { (*addr).sa_family };
    match sa_family as i32 {
        AF_INET => {
            if addrlen < mem::size_of::<SOCKADDR_IN>() as INT {
                return None;
            }
            let addr_in = unsafe { &*(addr as *const SOCKADDR_IN) };
            let ip =
                unsafe { std::net::Ipv4Addr::from(u32::from_be(*addr_in.sin_addr.S_un.S_addr())) };
            let port = u16::from_be(addr_in.sin_port);
            Some(SocketAddr::new(ip.into(), port))
        }
        AF_INET6 => {
            if addrlen < mem::size_of::<SOCKADDR_IN6>() as INT {
                return None;
            }
            let addr_in6 = unsafe { &*(addr as *const SOCKADDR_IN6) };
            let ip = unsafe { std::net::Ipv6Addr::from(*addr_in6.sin6_addr.u.Byte()) };
            let port = u16::from_be(addr_in6.sin6_port);
            Some(SocketAddr::new(ip.into(), port))
        }
        _ => None,
    }
}

/// Convert SocketAddr to Windows SOCKADDR for address return functions
///
/// # Safety
/// `name` and `namelen` must be valid writable pointers, and the caller must provide enough space
/// in `name` for the resulting `SOCKADDR`.
#[cfg(windows)]
pub unsafe fn socketaddr_to_windows_sockaddr(
    addr: &SocketAddr,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> WindowsResult<()> {
    if name.is_null() || namelen.is_null() {
        return Err(WindowsError::WinSock(WSAEFAULT));
    }

    let name_len_value = unsafe { *namelen };
    if name_len_value < 0 {
        return Err(WindowsError::WinSock(WSAEFAULT));
    }

    match addr {
        SocketAddr::V4(addr_v4) => {
            let size = mem::size_of::<SOCKADDR_IN>() as INT;
            if name_len_value < size {
                return Err(WindowsError::WinSock(WSAEFAULT));
            }

            let mut sockaddr_in: SOCKADDR_IN = unsafe { mem::zeroed() };
            sockaddr_in.sin_family = AF_INET as u16;
            sockaddr_in.sin_port = addr_v4.port().to_be();
            unsafe {
                *sockaddr_in.sin_addr.S_un.S_addr_mut() = u32::from(*addr_v4.ip()).to_be();
            }

            unsafe {
                std::ptr::copy_nonoverlapping(
                    &sockaddr_in as *const _ as *const u8,
                    name as *mut u8,
                    size as usize,
                );
                *namelen = size;
            }

            Ok(())
        }
        SocketAddr::V6(addr_v6) => {
            let size = mem::size_of::<SOCKADDR_IN6>() as INT;
            if name_len_value < size {
                return Err(WindowsError::WinSock(WSAEFAULT));
            }

            let mut sockaddr_in6: SOCKADDR_IN6 = unsafe { mem::zeroed() };
            sockaddr_in6.sin6_family = AF_INET6 as u16;
            sockaddr_in6.sin6_port = addr_v6.port().to_be();
            sockaddr_in6.sin6_flowinfo = addr_v6.flowinfo();
            // Note: scope_id is not available in SOCKADDR_IN6_LH

            // Copy the IPv6 address bytes
            let ip_bytes = addr_v6.ip().octets();
            unsafe {
                let bytes_ptr = sockaddr_in6.sin6_addr.u.Byte().as_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(ip_bytes.as_ptr(), bytes_ptr, 16);
            }

            unsafe {
                std::ptr::copy_nonoverlapping(
                    &sockaddr_in6 as *const _ as *const u8,
                    name as *mut u8,
                    size as usize,
                );
                *namelen = size;
            }

            Ok(())
        }
    }
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
    #[mirrord_layer_macro::instrument(level = "trace", ret)]
    pub fn get_connection_through(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
    ) -> HookResult<ConnectionThrough> {
        let (filters, selector_is_local) = match self {
            Self::Unfiltered => return Ok(ConnectionThrough::Remote(address)),
            Self::Local(filters) => (filters, true),
            Self::Remote(filters) => (filters, false),
        };

        for filter in filters {
            if !filter.matches(address, protocol, selector_is_local)? {
                continue;
            }

            return if selector_is_local {
                Self::get_local_address_to_connect(address).map(ConnectionThrough::Local)
            } else {
                Ok(ConnectionThrough::Remote(address))
            };
        }

        if selector_is_local {
            Ok(ConnectionThrough::Remote(address))
        } else {
            Self::get_local_address_to_connect(address).map(ConnectionThrough::Local)
        }
    }

    /// Helper function that looks into
    /// [`reverse_dns::REMOTE_DNS_REVERSE_MAPPING`](crate::socket::dns::reverse_dns::REMOTE_DNS_REVERSE_MAPPING)
    /// for `address`, so we can
    /// retrieve the hostname and resolve it locally (when applicable).
    ///
    /// - `address`: the [`SocketAddr`] that was passed to `connect`;
    ///
    /// We only get here when the [`OutgoingSelector::Remote`] matched nothing, or when the
    /// [`OutgoingSelector::Local`] matched on something.
    ///
    /// Returns 1 of 2 possibilities:
    ///
    /// 1. `address` is in
    /// [`reverse_dns::REMOTE_DNS_REVERSE_MAPPING`](crate::socket::dns::reverse_dns::REMOTE_DNS_REVERSE_MAPPING):
    ///    resolves the hostname locally, then
    /// return the first result
    /// 2. `address` is **NOT** in
    /// [`reverse_dns::REMOTE_DNS_REVERSE_MAPPING`](crate::socket::dns::reverse_dns::REMOTE_DNS_REVERSE_MAPPING):
    ///    return the `address` as is;
    #[mirrord_layer_macro::instrument(level = "trace", ret)]
    fn get_local_address_to_connect(address: SocketAddr) -> HookResult<SocketAddr> {
        // Aviram: I think this whole function and logic is weird but I really need to get
        // https://github.com/metalbear-co/mirrord/issues/2389 fixed and I don't have time to
        // fully understand or refactor, and the logic is sound (if it's loopback, just connect to
        // it)
        if address.ip().is_loopback() {
            return Ok(address);
        }

        let cached = get_hostname_for_ip(address.ip());
        let Some(hostname) = cached else {
            return Ok(address);
        };

        #[cfg(unix)]
        let _guard = DetourGuard::new();
        (hostname, address.port())
            .to_socket_addrs()?
            .next()
            .ok_or(HookError::DNSNoName)
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

/// [`ProtocolAndAddressFilter`] extension.
trait ProtocolAndAddressFilterExt {
    /// Matches the outgoing connection request (given as [[`SocketAddr`], [`NetProtocol`]] pair)
    /// against this filter.
    ///
    /// # Note on DNS resolution
    ///
    /// This method may require a DNS resolution (when [`ProtocolAndAddressFilter::address`] is
    /// [`AddressFilter::Name`]). If remote DNS is disabled or `force_local_dns`
    /// flag is used, the method uses local resolution [`ToSocketAddrs`]. Otherwise, it uses
    /// remote resolution [`remote_getaddrinfo`].
    fn matches(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
        force_local_dns: bool,
    ) -> HookResult<bool>;
}

impl ProtocolAndAddressFilterExt for ProtocolAndAddressFilter {
    fn matches(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
        force_local_dns: bool,
    ) -> HookResult<bool> {
        if let (ProtocolFilter::Tcp, NetProtocol::Datagrams)
        | (ProtocolFilter::Udp, NetProtocol::Stream) = (self.protocol, protocol)
        {
            return Ok(false);
        };

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
                let resolved_ips = if setup().remote_dns_enabled() && !force_local_dns {
                    match remote_getaddrinfo(name.to_string(), *port, 0, family, 0, addr_protocol) {
                        Ok(res) => res.into_iter().map(|(_, ip)| ip).collect(),
                        Err(HookError::ResponseError(ResponseError::DnsLookup(
                            DnsLookupError {
                                kind: ResolveErrorKindInternal::NoRecordsFound(..),
                            },
                        ))) => vec![],
                        Err(e) => {
                            tracing::error!(error = ?e, "Remote resolution of OutgoingFilter failed");
                            return Err(e);
                        }
                    }
                } else {
                    #[cfg(unix)]
                    let _guard = DetourGuard::new();

                    // Use standard library DNS resolution as fallback
                    match (name.as_str(), *port).to_socket_addrs() {
                        Ok(addresses) => addresses.map(|addr| addr.ip()).collect(),
                        Err(e) => {
                            let as_string = e.to_string();
                            if as_string.contains("Temporary failure in name resolution")
                                || as_string
                                    .contains("nodename nor servname provided, or not known")
                            {
                                // There is no special `ErrorKind` for case when no records are
                                // found. We catch this case based
                                // on error message.
                                vec![]
                            } else {
                                tracing::error!(error = ?e, "Local resolution of OutgoingFilter failed");
                                return Err(e.into());
                            }
                        }
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
