//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(not(target_os = "windows"))]
use libc::{c_int, sockaddr, socklen_t};
// Re-export shared types from layer-lib
pub(crate) use mirrord_layer_lib::socket::{
    Bound, Connected, ConnectionThrough, DnsResolver, OutgoingSelector, SocketKind, SocketState,
    UserSocket, is_ignored_port,
};
use mirrord_protocol::{
    DnsLookupError, ResolveErrorKindInternal, ResponseError, outgoing::SocketAddress,
};
use socket2::SockAddr;
use tracing::warn;

#[cfg(not(target_os = "windows"))]
use crate::socket::ops::remote_getaddrinfo;
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
#[cfg(not(target_os = "windows"))]
pub(crate) mod outgoing_selector;

/// Stores the [`UserSocket`]s created by the user.
///
/// **Warning**: Do not put logs in here! If you try logging stuff inside this initialization
/// you're gonna have a bad time. The process hanging is the min you should expect, if you
/// choose to ignore this warning.
///
/// - [`SHARED_SOCKETS_ENV_VAR`]: Some sockets may have been initialized by a parent process
///   through [`libc::execve`] (or any `exec*`), and the spawned children may want to use those
///   sockets. As memory is not shared via `exec*` calls (unlike `fork`), we need a way to pass
///   parent sockets to child processes. The way we achieve this is by setting the
///   [`SHARED_SOCKETS_ENV_VAR`] with an [`BASE64_URL_SAFE`] encoded version of our
///   [`SOCKETS`]. The env var is set as `MIRRORD_SHARED_SOCKETS=({fd}, {UserSocket}),*`.
///
/// - [`libc::FD_CLOEXEC`] behaviour: While rebuilding sockets from the env var, we also check
///   if they're set with the cloexec flag, so that children processes don't end up using
///   sockets that are exclusive for their parents.
// Re-export the unified SOCKETS from layer-lib
pub(crate) use mirrord_layer_lib::socket::{SHARED_SOCKETS_ENV_VAR, SOCKETS};

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
