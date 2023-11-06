//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    os::unix::io::RawFd,
    str::FromStr,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use hashbrown::hash_set::HashSet;
use libc::{c_int, sockaddr, socklen_t};
use mirrord_config::feature::network::outgoing::{
    AddressFilter, OutgoingConfig, OutgoingFilter, OutgoingFilterConfig, ProtocolFilter,
};
use mirrord_intproxy_protocol::{NetProtocol, PortUnsubscribe};
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;
use tracing::warn;
use trust_dns_resolver::config::Protocol;

use self::id::SocketId;
use crate::{
    common,
    detour::{Bypass, Detour, DetourGuard, OptionExt},
    error::{HookError, HookResult},
    socket::ops::{remote_getaddrinfo, REMOTE_DNS_REVERSE_MAPPING},
};

pub(super) mod hooks;
pub(crate) mod id;
pub(crate) mod ops;

pub(crate) static SOCKETS: LazyLock<DashMap<RawFd, Arc<UserSocket>>> = LazyLock::new(DashMap::new);

/// Contains the addresses of a mirrord connected socket.
///
/// - `layer_address` is only used for the outgoing feature.
#[derive(Debug)]
pub struct Connected {
    /// The address requested by the user that we're "connected" to.
    ///
    /// Whenever the user calls [`libc::getpeername`], this is the address we return to them.
    ///
    /// For the _outgoing_ feature, we actually connect to the `layer_address` interceptor socket,
    /// but use this address in the [`libc::recvfrom`] handling of [`fill_address`].
    remote_address: SocketAddress,

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
    /// We would set this ip as `1.2.3.4:{port}` in [`bind`], where `{port}` is the user requested
    /// port.
    local_address: SocketAddress,

    /// The address of the interceptor socket, this is what we're really connected to in the
    /// outgoing feature.
    layer_address: Option<SocketAddress>,
}

/// Represents a [`SocketState`] where the user made a [`libc::bind`] call, and we intercepted it.
///
/// ## Details
///
/// Our [`ops::bind`] hook doesn't bind the address that the user passed to us, instead we call
/// [`hooks::FN_BIND`] with `localhost:0` (or `unspecified:0` for ipv6), and use
/// [`hooks::FN_GETSOCKNAME`] to retrieve this bound address which we assign to `Bound::address`.
///
/// The original user requested address is assigned to `Bound::requested_address`, and used as an
/// illusion for when the user calls [`libc::getsockname`], as if this address was the actual local
/// bound address.
#[derive(Debug, Clone, Copy)]
pub struct Bound {
    /// Address originally requested by the user for [`bind`].
    requested_address: SocketAddr,

    /// Actual bound address that we use to communicate between the user's listener socket and our
    /// interceptor socket.
    address: SocketAddr,
}

#[derive(Debug, Default)]
pub enum SocketState {
    #[default]
    Initialized,
    Bound(Bound),
    Listening(Bound),
    Connected(Connected),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketKind {
    Tcp(c_int),
    Udp(c_int),
}

impl SocketKind {
    pub(crate) const fn is_udp(self) -> bool {
        matches!(self, Self::Udp(..))
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
    type Error = Bypass;

    fn try_from(type_: c_int) -> Result<Self, Self::Error> {
        if (type_ & libc::SOCK_STREAM) > 0 {
            Ok(SocketKind::Tcp(type_))
        } else if (type_ & libc::SOCK_DGRAM) > 0 {
            Ok(SocketKind::Udp(type_))
        } else {
            Err(Bypass::Type(type_))
        }
    }
}

// TODO(alex): We could treat `sockfd` as being the same as `&self` for socket ops, we currently
// can't do that due to how `dup` interacts directly with our `Arc<UserSocket>`, because we just
// `clone` the arc, we end up with exact duplicates, but `dup` generates a new fd that we have no
// way of putting inside the duplicated `UserSocket`.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct UserSocket {
    pub(crate) id: SocketId,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    pub state: SocketState,
    pub(crate) kind: SocketKind,
}

impl UserSocket {
    pub(crate) fn new(
        domain: c_int,
        type_: c_int,
        protocol: c_int,
        state: SocketState,
        kind: SocketKind,
    ) -> Self {
        Self {
            id: Default::default(),
            domain,
            type_,
            protocol,
            state,
            kind,
        }
    }

    /// Inform internal proxy about closing a listening port.
    #[tracing::instrument(level = "trace", ret)]
    pub(crate) fn close(&self) {
        if let Self {
            state: SocketState::Listening(bound),
            kind: SocketKind::Tcp(..),
            ..
        } = self
        {
            let _ = common::make_proxy_request_no_response(PortUnsubscribe {
                port: bound.requested_address.port(),
            });
        }
    }
}

/// Holds valid address that we should use to `connect_outgoing`.
#[derive(Debug, Clone, Copy)]
enum ConnectionThrough {
    /// Connect locally, this means just call `FN_CONNECT` on the inner [`SokcetAddr`].
    Local(SocketAddr),

    /// Connect through the agent.
    Remote(SocketAddr),
}

/// Holds the [`OutgoingFilter`]s set up by the user.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum OutgoingSelector {
    #[default]
    Unfiltered,
    /// If the address from `connect` matches this, then we send the connection through the
    /// remote pod.
    Remote(HashSet<OutgoingFilter>),
    /// If the address from `connect` matches this, then we send the connection from the local app.
    Local(HashSet<OutgoingFilter>),
}

impl OutgoingSelector {
    fn build_selector<'a, I: Iterator<Item = &'a str>>(
        filters: I,
        tcp_enabled: bool,
        udp_enabled: bool,
    ) -> HashSet<OutgoingFilter> {
        filters
            .map(|filter| OutgoingFilter::from_str(filter).expect("invalid outgoing filter"))
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|OutgoingFilter { protocol, .. }| match protocol {
                ProtocolFilter::Any => tcp_enabled || udp_enabled,
                ProtocolFilter::Tcp => tcp_enabled,
                ProtocolFilter::Udp => udp_enabled,
            })
            .collect::<HashSet<_>>()
    }

    /// Builds a new instance from the user config, removing filters
    /// that would create inconsitencies, by checking if their protocol is enabled for outgoing
    /// traffic, and thus we avoid making this check on every [`ops::connect`] call.
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
                list.as_slice().iter().map(String::as_str),
                config.tcp,
                config.udp,
            )),
            Some(OutgoingFilterConfig::Local(list)) => Self::Local(Self::build_selector(
                list.as_slice().iter().map(String::as_str),
                config.tcp,
                config.udp,
            )),
        }
    }

    /// Checks if the `address` matches the specified outgoing filter.
    ///
    /// Returns either a [`ConnectionThrough::Remote`] or [`ConnectionThroughLocal`], with the
    /// address that should be connected to.
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
    #[tracing::instrument(level = "trace", ret)]
    fn get_connection_through(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
    ) -> Detour<ConnectionThrough> {
        // Closure that checks if the current filter matches the enabled protocols.
        let filter_protocol = move |outgoing: &&OutgoingFilter| {
            matches!(
                (outgoing.protocol, protocol),
                (ProtocolFilter::Any, _)
                    | (ProtocolFilter::Tcp, NetProtocol::Stream)
                    | (ProtocolFilter::Udp, NetProtocol::Datagrams)
            )
        };

        // Closure to skip hostnames, as these filters will be dealt with after being resolved.
        let skip_unresolved =
            |outgoing: &&OutgoingFilter| !matches!(outgoing.address, AddressFilter::Name(_));

        // Closure that tries to match `address` with something in the selector set.
        let any_address = |outgoing: &OutgoingFilter| match outgoing.address {
            AddressFilter::Socket(select_address) => {
                (select_address.ip().is_unspecified() && select_address.port() == 0)
                    || (select_address.ip().is_unspecified()
                        && select_address.port() == address.port())
                    || (select_address.port() == 0 && select_address.ip() == address.ip())
                    || select_address == address
            }
            // TODO(alex): We could enforce this at the type level, by converting `OutgoingWhatever`
            // to a type that doesn't have `AddressFilter::Name`.
            AddressFilter::Name(_) => unreachable!("BUG: We skip these in the iterator!"),
            AddressFilter::Subnet((subnet, port)) => {
                subnet.contains(&address.ip()) && (port == 0 || port == address.port())
            }
        };

        // Resolve the hostnames in the filter.
        let resolved_hosts = match &self {
            OutgoingSelector::Unfiltered => HashSet::default(),
            OutgoingSelector::Remote(_) => self.resolve_dns::<true>()?,
            OutgoingSelector::Local(_) => self.resolve_dns::<false>()?,
        };
        let hosts = resolved_hosts.iter();

        // Return the valid address to connect, in some cases (when DNS resolving is involved),
        // this address may be different than the one we initially received in this function.
        Detour::Success(match self {
            OutgoingSelector::Unfiltered => ConnectionThrough::Remote(address),
            OutgoingSelector::Remote(list) => {
                if !list
                    .iter()
                    .filter(skip_unresolved)
                    .chain(hosts)
                    .filter(filter_protocol)
                    .any(any_address)
                {
                    // No "remote" selector matched `address`, so now we try to get the correct
                    // address to connect to, either it's a resolved hostname, then we check our
                    // `REMOTE_DNS_REVERSE_MAPPING` and resolve the hostname locally, or this
                    // `address` is the one the user wants.
                    Self::get_local_address_to_connect(address)?
                } else {
                    ConnectionThrough::Remote(address)
                }
            }
            OutgoingSelector::Local(list) => {
                if list
                    .iter()
                    .filter(skip_unresolved)
                    .chain(hosts)
                    .filter(filter_protocol)
                    .any(any_address)
                {
                    // Our "local" selector matched (e.g. because of the port), but this `address`
                    // might be a remotely resolved ip, so we first have to
                    // check in our `REMOTE_DNS_REVERSE_MAPPING` to resolve
                    // the original hostname locally, and get a local ip we can connect to.
                    Self::get_local_address_to_connect(address)?
                } else {
                    ConnectionThrough::Remote(address)
                }
            }
        })
    }

    /// Resolves the [`OutgoingFilter`] that are host names using either [`remote_getaddrinfo`] or
    /// regular `getaddrinfo`, depending if the user set up the `dns` feature to resolve DNS through
    /// the remote pod or not.
    ///
    /// The resolved values are returned in a set as `AddressFilter::Socket`.
    ///
    /// `REMOTE` controls whether the named hosts should be resolved remotely, by checking if we're
    /// dealing with [`OutgoingSelector::Remote`] and [`REMOTE_DNS`] is set.
    #[tracing::instrument(level = "debug", ret)]
    fn resolve_dns<const REMOTE: bool>(&self) -> HookResult<HashSet<OutgoingFilter>> {
        // Closure that tries to match `address` with something in the selector set.
        let is_name =
            |outgoing: &&OutgoingFilter| matches!(outgoing.address, AddressFilter::Name(_));

        // Converts `AddressFilter::Name`s into something more convenient to be used in `resolve`.
        let to_name_and_port = |outgoing: &OutgoingFilter| match &outgoing.address {
            AddressFilter::Name((name, port)) => (outgoing.protocol, name.clone(), *port),
            _ => unreachable!("Filter went wrong, we should only have named addresses here!"),
        };

        // Resolves a list of host names, depending on how the user sets the remote `dns` feature.
        let resolve = |unresolved: HashSet<(ProtocolFilter, String, u16)>| {
            const USUAL_AMOUNT_OF_ADDRESSES: usize = 8;
            let amount_of_addresses = unresolved.len() * USUAL_AMOUNT_OF_ADDRESSES;
            let mut unresolved = unresolved.into_iter();

            let resolved = if crate::setup().remote_dns_enabled() && REMOTE {
                // Resolve DNS through the agent.
                unresolved
                    .try_fold(
                        HashSet::with_capacity(amount_of_addresses),
                        |mut resolved, (protocol, name, port)| {
                            let addresses =
                                remote_getaddrinfo(name)?.into_iter().map(|(_, address)| {
                                    OutgoingFilter {
                                        protocol,
                                        address: AddressFilter::Socket(SocketAddr::new(
                                            address, port,
                                        )),
                                    }
                                });

                            resolved.extend(addresses);
                            Ok::<_, HookError>(resolved)
                        },
                    )?
                    .into_iter()
            } else {
                // Resolve DNS locally.
                unresolved
                    .try_fold(
                        HashSet::with_capacity(amount_of_addresses),
                        |mut resolved: HashSet<OutgoingFilter>, (protocol, name, port)| {
                            let addresses =
                                format!("{name}:{port}").to_socket_addrs()?.map(|address| {
                                    OutgoingFilter {
                                        protocol,
                                        address: AddressFilter::Socket(SocketAddr::new(
                                            address.ip(),
                                            port,
                                        )),
                                    }
                                });

                            resolved.extend(addresses);
                            Ok::<_, HookError>(resolved)
                        },
                    )?
                    .into_iter()
            };

            Ok::<_, HookError>(resolved)
        };

        match self {
            OutgoingSelector::Unfiltered => Ok(HashSet::new()),
            OutgoingSelector::Remote(filter) | OutgoingSelector::Local(filter) => Ok(resolve(
                filter
                    .iter()
                    .filter(is_name)
                    .map(to_name_and_port)
                    .collect(),
            )?
            .collect()),
        }
    }

    /// Helper function that looks into the [`REMOTE_DNS_REVERSE_MAPPING`] for `address`, so we can
    /// retrieve the hostname and resolve it locally (when applicable).
    ///
    /// - `address`: the [`SocketAddr`] that was passed to `connect`;
    ///
    /// We only get here when the [`OutgoingSelector::Remote`] matched nothing, or when the
    /// [`OutgoingSelector::Local`] matched on something.
    ///
    /// Returns 1 of 2 possibilities:
    ///
    /// 1. `address` is in [`REMOTE_DNS_REVERSE_MAPPING`]: resolves the hostname locally, then
    /// return it as [`ConnectionThrough::Local`];
    /// 2. `address` is **NOT** in [`REMOTE_DNS_REVERSE_MAPPING`]: return the `address` as-is;
    #[tracing::instrument(level = "trace", ret)]
    fn get_local_address_to_connect(address: SocketAddr) -> Detour<ConnectionThrough> {
        if let Some((cached_hostname, port)) = REMOTE_DNS_REVERSE_MAPPING
            .get(&address.ip())
            .map(|addr| (addr.value().clone(), address.port()))
        {
            let _guard = DetourGuard::new();
            let locally_resolved = format!("{cached_hostname}:{port}")
                .to_socket_addrs()?
                .find(SocketAddr::is_ipv4)?;

            Detour::Success(ConnectionThrough::Local(locally_resolved))
        } else {
            Detour::Success(ConnectionThrough::Local(address))
        }
    }
}

#[inline]
fn is_ignored_port(addr: &SocketAddr) -> bool {
    let (ip, port) = (addr.ip(), addr.port());
    let ignored_ip = ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || ip == IpAddr::V6(Ipv6Addr::LOCALHOST);
    port == 0 || ignored_ip && (port > 50000 && port < 60000)
}

/// Fill in the sockaddr structure for the given address.
#[inline]
fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SockAddr,
) -> Detour<i32> {
    let result = if address.is_null() {
        Ok(0)
    } else if address_len.is_null() {
        Err(HookError::NullPointer)
    } else {
        unsafe {
            let len = std::cmp::min(*address_len as usize, new_address.len() as usize);

            std::ptr::copy_nonoverlapping(
                new_address.as_ptr() as *const u8,
                address as *mut u8,
                len,
            );
            *address_len = new_address.len();
        }

        Ok(0)
    }?;

    Detour::Success(result)
}

pub(crate) trait ProtocolExt {
    fn try_from_raw(ai_protocol: i32) -> HookResult<Protocol>;
    fn try_into_raw(self) -> HookResult<i32>;
}

impl ProtocolExt for Protocol {
    fn try_from_raw(ai_protocol: i32) -> HookResult<Self> {
        match ai_protocol {
            libc::IPPROTO_UDP => Ok(Protocol::Udp),
            libc::IPPROTO_TCP => Ok(Protocol::Tcp),
            libc::IPPROTO_SCTP => todo!(),
            other => {
                warn!("Trying a protocol of {:#?}", other);
                Ok(Protocol::Tcp)
            }
        }
    }

    fn try_into_raw(self) -> HookResult<i32> {
        match self {
            Protocol::Udp => Ok(libc::IPPROTO_UDP),
            Protocol::Tcp => Ok(libc::IPPROTO_TCP),
            _ => todo!(),
        }
    }
}

pub trait SocketAddrExt {
    /// Converts a raw [`sockaddr`] pointer into a more _Rusty_ type
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<Self>
    where
        Self: Sized;
}

impl SocketAddrExt for SockAddr {
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<SockAddr> {
        unsafe {
            SockAddr::try_init(|storage, len| {
                storage.copy_from_nonoverlapping(raw_address.cast(), 1);
                len.copy_from_nonoverlapping(&address_length, 1);

                Ok(())
            })
        }
        .ok()
        .map(|((), address)| address)
        .bypass(Bypass::AddressConversion)
    }
}

impl SocketAddrExt for SocketAddr {
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<SocketAddr> {
        SockAddr::try_from_raw(raw_address, address_length)
            .and_then(|address| address.as_socket().bypass(Bypass::AddressConversion))
    }
}
