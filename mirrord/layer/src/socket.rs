//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashSet, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    os::unix::io::RawFd,
    str::FromStr,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use libc::{c_int, sockaddr, socklen_t};
use mirrord_config::feature::network::outgoing::OutgoingFilter;
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;
use tracing::{debug, error, warn};
use trust_dns_resolver::config::Protocol;

use self::id::SocketId;
use crate::{
    common::{blocking_send_hook_message, HookMessage},
    detour::{Bypass, Detour, OptionExt},
    error::{HookError, HookResult, LayerError},
    ENABLED_TCP_OUTGOING, ENABLED_UDP_OUTGOING, INCOMING_IGNORE_PORTS,
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

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OutgoingProtocol {
    #[default]
    Any,
    Tcp,
    Udp,
}

impl FromStr for OutgoingProtocol {
    type Err = LayerError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lowercase = s.to_lowercase();

        match lowercase.as_str() {
            "any" => Ok(Self::Any),
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            _ => Err(LayerError::NoProcessFound),
        }
    }
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct OutgoingSelector {
    remote: HashSet<OutgoingFilter>,
    local: HashSet<OutgoingFilter>,
}

impl OutgoingSelector {
    pub(crate) fn new(remote: HashSet<OutgoingFilter>, local: HashSet<OutgoingFilter>) -> Self {
        use mirrord_config::feature::network::outgoing::*;

        let enabled_tcp = *ENABLED_TCP_OUTGOING
            .get()
            .expect("ENABLED_TCP_OUTGOING should be set before initializing OutgoingSelector!");

        let enabled_udp = *ENABLED_UDP_OUTGOING
            .get()
            .expect("ENABLED_TCP_OUTGOING should be set before initializing OutgoingSelector!");

        let remote = remote
            .into_iter()
            .filter(|OutgoingFilter { protocol, .. }| match protocol {
                ProtocolFilter::Any => enabled_tcp || enabled_udp,
                ProtocolFilter::Tcp => enabled_tcp,
                ProtocolFilter::Udp => enabled_udp,
            })
            .collect();

        let local = local
            .into_iter()
            .filter(|OutgoingFilter { protocol, .. }| match protocol {
                ProtocolFilter::Any => enabled_tcp || enabled_udp,
                ProtocolFilter::Tcp => enabled_tcp,
                ProtocolFilter::Udp => enabled_udp,
            })
            .collect();

        Self { remote, local }
    }

    #[tracing::instrument(level = "debug", ret)]
    fn connect_remote<const PROTOCOL: ConnectProtocol>(&self, address: SocketAddr) -> bool {
        use mirrord_config::feature::network::outgoing::{OutgoingAddress, ProtocolFilter};

        let filter_protocol = |outgoing: &&OutgoingFilter| match outgoing.protocol {
            ProtocolFilter::Any => true,
            ProtocolFilter::Tcp if PROTOCOL == TCP => true,
            ProtocolFilter::Udp if PROTOCOL == UDP => true,
            _ => false,
        };

        let any_address = |outgoing: &OutgoingFilter| match outgoing.address {
            OutgoingAddress::Socket(select_address) => {
                select_address.ip().is_unspecified()
                    || select_address.port() == 0
                    || select_address == address
            }
            OutgoingAddress::Name((ref name, port)) => format!("{name}:{port}")
                .to_socket_addrs()
                .inspect(|resolved| debug!("resolved addresses {resolved:#?}"))
                .inspect_err(|fail| error!("resolved addresses  failed {fail:#?}"))
                .map(|mut resolved| {
                    resolved.any(|resolved_address| {
                        resolved_address == SocketAddr::new(address.ip(), 0)
                    })
                })
                .unwrap_or_default(),
            OutgoingAddress::Subnet((subnet, port)) => {
                subnet.contains(&address.ip()) && (port == 0 || port == address.port())
            }
        };

        self.remote.iter().filter(filter_protocol).any(any_address)
            || !self.local.iter().filter(filter_protocol).any(any_address)
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
