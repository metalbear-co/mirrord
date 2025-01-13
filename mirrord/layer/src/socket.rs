//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    os::unix::io::RawFd,
    str::FromStr,
    sync::{Arc, LazyLock, Mutex},
};

use base64::prelude::*;
use bincode::{Decode, Encode};
use hashbrown::hash_set::HashSet;
use hooks::FN_FCNTL;
use libc::{c_int, sockaddr, socklen_t};
use mirrord_config::feature::network::{
    filter::{AddressFilter, ProtocolAndAddressFilter, ProtocolFilter},
    outgoing::{OutgoingConfig, OutgoingFilterConfig},
};
use mirrord_intproxy_protocol::{NetProtocol, PortUnsubscribe};
use mirrord_protocol::{
    outgoing::SocketAddress, DnsLookupError, ResolveErrorKindInternal, ResponseError,
};
use socket2::SockAddr;
use tracing::warn;

use crate::{
    common,
    detour::{Bypass, Detour, DetourGuard, OptionExt},
    error::{HookError, HookResult},
    socket::ops::{remote_getaddrinfo, REMOTE_DNS_REVERSE_MAPPING},
};

#[cfg(target_os = "macos")]
mod apple_dnsinfo;
pub(crate) mod dns_selector;
pub(super) mod hooks;
pub(crate) mod ops;

pub(crate) const SHARED_SOCKETS_ENV_VAR: &str = "MIRRORD_SHARED_SOCKETS";

/// Stores the [`UserSocket`]s created by the user.
///
/// **Warning**: Do not put logs in here! If you try logging stuff inside this initialization
/// you're gonna have a bad time. The process hanging is the min you should expect, if you
/// choose to ignore this warning.
///
/// - [`SHARED_SOCKETS_ENV_VAR`]: Some sockets may have been initialized by a parent process through
///   [`libc::execve`] (or any `exec*`), and the spawned children may want to use those sockets. As
///   memory is not shared via `exec*` calls (unlike `fork`), we need a way to pass parent sockets
///   to child processes. The way we achieve this is by setting the [`SHARED_SOCKETS_ENV_VAR`] with
///   an [`BASE64_URL_SAFE`] encoded version of our [`SOCKETS`]. The env var is set as
///   `MIRRORD_SHARED_SOCKETS=({fd}, {UserSocket}),*`.
///
/// - [`libc::FD_CLOEXEC`] behaviour: While rebuilding sockets from the env var, we also check if
///   they're set with the cloexec flag, so that children processes don't end up using sockets that
///   are exclusive for their parents.
pub(crate) static SOCKETS: LazyLock<Mutex<HashMap<RawFd, Arc<UserSocket>>>> = LazyLock::new(|| {
    std::env::var(SHARED_SOCKETS_ENV_VAR)
        .ok()
        .and_then(|encoded| {
            BASE64_URL_SAFE
                .decode(encoded.into_bytes())
                .inspect_err(|error| {
                    tracing::warn!(
                        ?error,
                        "failed decoding base64 value from {SHARED_SOCKETS_ENV_VAR}"
                    )
                })
                .ok()
        })
        .and_then(|decoded| {
            bincode::decode_from_slice::<Vec<(i32, UserSocket)>, _>(
                &decoded,
                bincode::config::standard(),
            )
            .inspect_err(|error| tracing::warn!(?error, "failed parsing shared sockets env value"))
            .ok()
        })
        .map(|(fds_and_sockets, _)| {
            Mutex::new(HashMap::from_iter(fds_and_sockets.into_iter().filter_map(
                |(fd, socket)| {
                    // Do not inherit sockets that are `FD_CLOEXEC`.
                    if unsafe { FN_FCNTL(fd, libc::F_GETFD, 0) != -1 } {
                        Some((fd, Arc::new(socket)))
                    } else {
                        None
                    }
                },
            )))
        })
        .unwrap_or_default()
});

/// Contains the addresses of a mirrord connected socket.
///
/// - `layer_address` is only used for the outgoing feature.
#[derive(Debug, Clone, Encode, Decode)]
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
    /// We would set this ip as `1.2.3.4:{port}` in `bind`, where `{port}` is the user requested
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
#[derive(Debug, Clone, Copy, Encode, Decode)]
pub struct Bound {
    /// Address originally requested by the user for `bind`.
    requested_address: SocketAddr,

    /// Actual bound address that we use to communicate between the user's listener socket and our
    /// interceptor socket.
    address: SocketAddr,
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
#[derive(Debug, Clone, Encode, Decode)]
#[allow(dead_code)]
pub(crate) struct UserSocket {
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
            domain,
            type_,
            protocol,
            state,
            kind,
        }
    }

    /// Inform internal proxy about closing a listening port.
    #[mirrord_layer_macro::instrument(level = "trace", fields(pid = std::process::id()), ret)]
    pub(crate) fn close(&self) {
        if let Self {
            state: SocketState::Listening(bound),
            kind: SocketKind::Tcp(..),
            ..
        } = self
        {
            let _ = common::make_proxy_request_no_response(PortUnsubscribe {
                port: bound.requested_address.port(),
                listening_on: bound.address,
            });
        }
    }
}

/// Holds valid address that we should use to `connect_outgoing`.
#[derive(Debug, Clone, Copy)]
enum ConnectionThrough {
    /// Connect locally, this means just call `FN_CONNECT` on the inner [`SocketAddr`].
    Local(SocketAddr),

    /// Connect through the agent.
    Remote(SocketAddr),
}

/// Holds the [`ProtocolAndAddressFilter`]s set up by the user in the [`OutgoingFilterConfig`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum OutgoingSelector {
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
    fn get_connection_through(
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
    /// return the first result
    /// 2. `address` is **NOT** in [`REMOTE_DNS_REVERSE_MAPPING`]: return the `address` as is;
    #[mirrord_layer_macro::instrument(level = "trace", ret)]
    fn get_local_address_to_connect(address: SocketAddr) -> HookResult<SocketAddr> {
        // Aviram: I think this whole function and logic is weird but I really need to get
        // https://github.com/metalbear-co/mirrord/issues/2389 fixed and I don't have time to
        // fully understand or refactor, and the logic is sound (if it's loopback, just connect to
        // it)
        if address.ip().is_loopback() {
            return Ok(address);
        }

        let cached = REMOTE_DNS_REVERSE_MAPPING
            .lock()?
            .get(&address.ip())
            .cloned();
        let Some(hostname) = cached else {
            return Ok(address);
        };

        let _guard = DetourGuard::new();
        (hostname, address.port())
            .to_socket_addrs()?
            .next()
            .ok_or(HookError::DNSNoName)
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

        let family = if address.is_ipv4() {
            libc::AF_INET
        } else {
            libc::AF_INET6
        };

        let addr_protocol = if matches!(protocol, NetProtocol::Stream) {
            libc::SOCK_STREAM
        } else {
            libc::SOCK_DGRAM
        };

        match &self.address {
            AddressFilter::Name(name, port) => {
                let resolved_ips = if crate::setup().remote_dns_enabled() && !force_local_dns {
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
                    let _guard = DetourGuard::new();

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

#[inline]
fn is_ignored_port(addr: &SocketAddr) -> bool {
    addr.port() == 0
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
                // storage and raw_address size is dynamic.
                (storage as *mut u8)
                    .copy_from_nonoverlapping(raw_address as *const u8, address_length as usize);
                *len = address_length;
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
