//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    os::unix::io::RawFd,
    str::FromStr,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use hashbrown::hash_set::HashSet;
use libc::{c_int, sockaddr, socklen_t};
use mirrord_config::{
    feature::network::outgoing::{
        AddressFilter, OutgoingFilter, OutgoingFilterConfig, ProtocolFilter,
    },
    util::VecOrSingle,
};
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;
use tracing::warn;
use trust_dns_resolver::config::Protocol;

use self::id::SocketId;
use crate::{
    common::{blocking_send_hook_message, HookMessage},
    detour::{Bypass, Detour, OptionExt},
    error::{HookError, HookResult, LayerError},
    socket::ops::remote_getaddrinfo,
    ENABLED_TCP_OUTGOING, ENABLED_UDP_OUTGOING, INCOMING_IGNORE_PORTS, REMOTE_DNS,
};

pub(super) mod hooks;
pub(crate) mod id;
pub(crate) mod ops;

// TODO(alex): Should be an enum, but to do so requires the `adt_const_params` feature, which also
// requires enabling `incomplete_features`.
type ConnectProtocol = bool;
const TCP: ConnectProtocol = false;
const UDP: ConnectProtocol = !TCP;

pub(crate) static SOCKETS: LazyLock<DashMap<RawFd, Arc<UserSocket>>> = LazyLock::new(DashMap::new);

/// Holds the connections that have yet to be [`accept`](ops::accept)ed.
///
/// ## Details
///
/// The connections here are added by
/// [`TcpHandler::create_local_stream`](crate::tcp::TcpHandler::create_local_stream) when the agent
/// sends us a [`NewTcpConnection`](mirrord_protocol::tcp::NewTcpConnection).
///
/// And they become part of the [`UserSocket`]'s [`SocketState`] when [`ops::accept`] is called.
///
/// Finally, we remove a socket's queue when the socket's `fd` is closed in
/// [`close_layer_fd`](crate::close_layer_fd).
pub static CONNECTION_QUEUE: LazyLock<ConnectionQueue> = LazyLock::new(ConnectionQueue::default);

/// Struct sent over the socket once created to pass metadata to the hook
#[derive(Debug)]
pub(super) struct SocketInformation {
    /// Address of the incoming peer
    pub remote_address: SocketAddr,

    /// Address of the local peer (our IP)
    pub local_address: SocketAddr,
}

/// poll_agent loop inserts connection data into this queue, and accept reads it.
#[derive(Debug, Default)]
pub struct ConnectionQueue {
    connections: DashMap<SocketId, VecDeque<SocketInformation>>,
}

impl ConnectionQueue {
    /// Adds a connection.
    ///
    /// See [`TcpHandler::create_local_stream`](crate::tcp::TcpHandler::create_local_stream).
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn add(&self, id: SocketId, info: SocketInformation) {
        self.connections.entry(id).or_default().push_back(info);
    }

    /// Pops the next connection to be handled from `Self`.
    ///
    /// See [`ops::accept].
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn pop_front(&self, id: SocketId) -> Option<SocketInformation> {
        self.connections.get_mut(&id)?.pop_front()
    }

    /// Removes the [`ConnectionQueue`] associated with the [`UserSocket`].
    ///
    /// See [`crate::close_layer_fd].
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn remove(&self, id: SocketId) -> Option<VecDeque<SocketInformation>> {
        self.connections.remove(&id).map(|(_, v)| v)
    }
}

impl SocketInformation {
    #[tracing::instrument(level = "trace")]
    pub fn new(remote_address: SocketAddr, local_address: SocketAddr) -> Self {
        Self {
            remote_address,
            local_address,
        }
    }
}

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
    pub(crate) const fn is_udp(&self) -> bool {
        match self {
            SocketKind::Tcp(_) => false,
            SocketKind::Udp(_) => true,
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

    /// Return the local (requested) port a bound/listening (NOT CONNECTED) user socket is
    /// conceptually bound to.
    ///
    /// So if the app tried to bind port 80, return `Some(80)` (even if we actually mapped it to
    /// some other port).
    ///
    /// `None` if socket is not bound yet, or if this socket is a connection socket.
    fn get_bound_port(&self) -> Option<u16> {
        match &self.state {
            SocketState::Bound(Bound {
                requested_address, ..
            })
            | SocketState::Listening(Bound {
                requested_address, ..
            }) => Some(requested_address.port()),
            _ => None,
        }
    }

    /// Inform TCP handler about closing a bound/listening port.
    #[tracing::instrument(level = "trace", ret)]
    pub(crate) fn close(&self) {
        if let Some(port) = self.get_bound_port() {
            match self.kind {
                SocketKind::Tcp(_) => {
                    // Ignoring errors here. We continue running, potentially without
                    // informing the layer's and agent's TCP handlers about the socket
                    // close. The agent might try to continue sending incoming
                    // connections/data.
                    let _ = blocking_send_hook_message(HookMessage::Tcp(
                        super::tcp::TcpIncoming::Close(port),
                    ));
                }
                // We don't do incoming UDP, so no need to notify anyone about this.
                SocketKind::Udp(_) => {}
            }
        }
    }
}

/// Holds the [`OutgoingFilter`]s set up by the user, after a little bit of checking, see
/// [`OutgoingSelector::new`].
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

impl TryFrom<Option<OutgoingFilterConfig>> for OutgoingSelector {
    type Error = LayerError;

    /// Builds the [`OutgoingSelector`] from the user config (list of filters), removing filters
    /// that would create inconsitencies, by checking if their protocol is enabled for outgoing
    /// traffic, and thus we avoid making this check on every [`ops::connect`] call.
    ///
    /// It also removes duplicated filters, by putting them into a [`HashSet`].
    fn try_from(value: Option<OutgoingFilterConfig>) -> Result<Self, Self::Error> {
        let enabled_tcp = *ENABLED_TCP_OUTGOING
            .get()
            .expect("ENABLED_TCP_OUTGOING should be set before initializing OutgoingSelector!");

        let enabled_udp = *ENABLED_UDP_OUTGOING
            .get()
            .expect("ENABLED_TCP_OUTGOING should be set before initializing OutgoingSelector!");

        let build_selector = |list: VecOrSingle<String>| {
            Ok::<_, LayerError>(
                list.to_vec()
                    .into_iter()
                    .map(|filter| OutgoingFilter::from_str(&filter))
                    .collect::<Result<HashSet<_>, _>>()?
                    .into_iter()
                    .filter(|OutgoingFilter { protocol, .. }| match protocol {
                        ProtocolFilter::Any => enabled_tcp || enabled_udp,
                        ProtocolFilter::Tcp => enabled_tcp,
                        ProtocolFilter::Udp => enabled_udp,
                    })
                    .collect::<HashSet<_>>(),
            )
        };

        match value {
            None => Ok(Self::Unfiltered),
            Some(OutgoingFilterConfig::Remote(list)) | Some(OutgoingFilterConfig::Local(list))
                if list.is_empty() =>
            {
                Err(LayerError::MissingConfigValue(
                    "Outgoing traffic filter cannot be empty!".to_string(),
                ))
            }
            Some(OutgoingFilterConfig::Remote(list)) => Ok(Self::Remote(build_selector(list)?)),
            Some(OutgoingFilterConfig::Local(list)) => Ok(Self::Local(build_selector(list)?)),
        }
    }
}

impl OutgoingSelector {
    /// Checks if the `address` matches the specified outgoing filter, and returns a `bool`
    /// indicating if this connection should go through the remote pod, or from the local app.
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
    /// `address` matches something specified in `local = [":7777"]`, then _negate_ it and return
    /// `false`, meaning we return "do not connect remote" (or "connect local" if you prefer
    /// positive thinking).
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
    fn connect_remote<const PROTOCOL: ConnectProtocol>(
        &self,
        address: SocketAddr,
    ) -> HookResult<bool> {
        let filter_protocol = |outgoing: &&OutgoingFilter| match outgoing.protocol {
            ProtocolFilter::Any => true,
            ProtocolFilter::Tcp if PROTOCOL == TCP => true,
            ProtocolFilter::Udp if PROTOCOL == UDP => true,
            _ => false,
        };

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

        let resolved_hosts = match &self {
            OutgoingSelector::Unfiltered => HashSet::default(),
            OutgoingSelector::Remote(_) => self.resolve_dns::<true>()?,
            OutgoingSelector::Local(_) => self.resolve_dns::<false>()?,
        };
        let hosts = resolved_hosts.iter();

        Ok(match self {
            OutgoingSelector::Unfiltered => true,
            OutgoingSelector::Remote(list) => list
                .iter()
                .filter(skip_unresolved)
                .chain(hosts)
                .filter(filter_protocol)
                .any(any_address),
            OutgoingSelector::Local(list) => !list
                .iter()
                .filter(skip_unresolved)
                .chain(hosts)
                .filter(filter_protocol)
                .any(any_address),
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
    #[tracing::instrument(level = "trace", ret)]
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

            // Resolve DNS through the agent.
            let resolved = if *REMOTE_DNS.get().expect("REMOTE_DNS should be set by now!") && REMOTE
            {
                unresolved
                    .try_fold(
                        HashSet::with_capacity(amount_of_addresses),
                        |mut resolved, (protocol, name, port)| {
                            let addresses =
                                remote_getaddrinfo(name, port)?
                                    .into_iter()
                                    .map(|(_, address)| OutgoingFilter {
                                        protocol,
                                        address: AddressFilter::Socket(SocketAddr::new(
                                            address.ip(),
                                            port,
                                        )),
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
                            let addresses = name.to_socket_addrs()?.map(|address| OutgoingFilter {
                                protocol,
                                address: AddressFilter::Socket(SocketAddr::new(address.ip(), port)),
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

/// Trait that expands `std` and `socket2` sockets.
pub(crate) trait SocketAddrExt {
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
