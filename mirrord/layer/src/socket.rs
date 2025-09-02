//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
#[cfg(not(target_os = "windows"))]
use std::os::unix::io::RawFd;
use std::{
    net::{SocketAddr, ToSocketAddrs},
};

use base64::prelude::*;
use bincode::{Decode, Encode};
use hooks::FN_FCNTL;
#[cfg(not(target_os = "windows"))]
use libc::{c_int, sockaddr, socklen_t};
use mirrord_config::feature::network::{
    filter::{AddressFilter, ProtocolAndAddressFilter, ProtocolFilter},
    outgoing::{OutgoingConfig, OutgoingFilterConfig},
};
use mirrord_intproxy_protocol::{NetProtocol, PortUnsubscribe};
// Re-export shared types from layer-lib
pub(crate) use mirrord_layer_lib::socket::{
    Bound, Connected, ConnectionThrough, DnsResolver, OutgoingSelector, SocketAddrExt, SocketKind,
    SocketState, UserSocket, is_ignored_port,
};
use mirrord_protocol::{
    DnsLookupError, ResolveErrorKindInternal, ResponseError, outgoing::SocketAddress,
};
use socket2::SockAddr;
use tracing::warn;

#[cfg(not(target_os = "windows"))]
use crate::socket::ops::{REMOTE_DNS_REVERSE_MAPPING, remote_getaddrinfo};
use crate::{
    common,
    detour::{Bypass, Detour, DetourGuard, OptionExt},
    error::{HookError, HookResult},
};

#[cfg(target_os = "macos")]
mod apple_dnsinfo;
pub(crate) mod dns_selector;
pub(super) mod hooks;
pub(crate) mod ops;

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
// Re-export the unified SOCKETS from layer-lib
pub(crate) use mirrord_layer_lib::socket::{SOCKETS, SHARED_SOCKETS_ENV_VAR};

// Unix-specific extensions for UserSocket
impl UserSocket {
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

// Unix-specific SocketKind conversion
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

/// Unix-specific DNS resolver implementation
pub struct UnixDnsResolver;

impl DnsResolver for UnixDnsResolver {
    type Error = HookError;

    fn resolve_hostname(
        &self,
        hostname: &str,
        port: u16,
        family: i32,
        protocol: i32,
    ) -> Result<Vec<std::net::IpAddr>, Self::Error> {
        if crate::setup().remote_dns_enabled() {
            match remote_getaddrinfo(hostname.to_string(), port, 0, family, 0, protocol) {
                Ok(res) => Ok(res.into_iter().map(|(_, ip)| ip).collect()),
                Err(HookError::ResponseError(ResponseError::DnsLookup(DnsLookupError {
                    kind: ResolveErrorKindInternal::NoRecordsFound(..),
                }))) => Ok(vec![]),
                Err(e) => {
                    tracing::error!(error = ?e, "Remote resolution of OutgoingFilter failed");
                    Err(e)
                }
            }
        } else {
            let _guard = DetourGuard::new();
            match (hostname, port).to_socket_addrs() {
                Ok(addresses) => Ok(addresses.map(|addr| addr.ip()).collect()),
                Err(e) => {
                    let as_string = e.to_string();
                    if as_string.contains("Temporary failure in name resolution")
                        || as_string.contains("nodename nor servname provided, or not known")
                    {
                        // No records found
                        Ok(vec![])
                    } else {
                        tracing::error!(error = ?e, "Local resolution of OutgoingFilter failed");
                        Err(e.into())
                    }
                }
            }
        }
    }

    fn remote_dns_enabled(&self) -> bool {
        crate::setup().remote_dns_enabled()
    }
}

// Unix-specific extensions for OutgoingSelector with DNS resolution
impl OutgoingSelector {
    /// Unix-specific get_connection_through with full DNS resolution
    #[mirrord_layer_macro::instrument(level = "trace", ret)]
    pub(crate) fn get_connection_through(
        &self,
        address: SocketAddr,
        protocol: NetProtocol,
    ) -> HookResult<ConnectionThrough> {
        let resolver = UnixDnsResolver;

        let result = self.get_connection_through_with_resolver(address, protocol, &resolver)?;

        // Apply Unix-specific address resolution for local connections
        match result {
            ConnectionThrough::Local(addr) => {
                Self::get_local_address_to_connect(addr).map(ConnectionThrough::Local)
            }
            ConnectionThrough::Remote(addr) => Ok(ConnectionThrough::Remote(addr)),
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

// Unix-specific SocketAddrExt trait with libc types
pub trait SocketAddrExtUnix {
    /// Converts a raw [`sockaddr`] pointer into a more _Rusty_ type
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<Self>
    where
        Self: Sized;
}

impl SocketAddrExtUnix for SockAddr {
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

impl SocketAddrExtUnix for SocketAddr {
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<SocketAddr> {
        SockAddr::try_from_raw(raw_address, address_length)
            .and_then(|address| address.as_socket().bypass(Bypass::AddressConversion))
    }
}

/// Fill in the sockaddr structure for the given address (Unix-specific version).
#[inline]
pub(crate) fn fill_address(
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
