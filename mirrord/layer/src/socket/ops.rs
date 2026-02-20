use alloc::ffi::CString;
use core::{ffi::CStr, mem};
#[cfg(target_os = "macos")]
use std::os::fd::BorrowedFd;
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, TcpStream},
    ops::Not,
    os::{
        fd::{FromRawFd, IntoRawFd},
        unix::io::RawFd,
    },
    path::PathBuf,
    ptr::{self, copy_nonoverlapping},
    sync::{Arc, OnceLock},
};

use libc::{AF_UNIX, c_int, c_void, hostent, sockaddr, socklen_t};
#[cfg(target_os = "macos")]
use libc::{SAE_ASSOCID_ANY, c_uint, iovec, sa_endpoints_t, sae_associd_t, sae_connid_t, size_t};
use mirrord_config::feature::network::incoming::{IncomingConfig, IncomingMode};
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, NetProtocol, OutgoingConnMetadataRequest,
    PortSubscribe,
};
#[cfg(target_os = "macos")]
use mirrord_layer_lib::socket::apple_dnsinfo::*;
use mirrord_layer_lib::{
    detour::{Detour, OnceLockExt, OptionExt},
    error::{HookError, HookResult},
    graceful_exit,
    proxy_connection::make_proxy_request_with_response,
    socket::{
        Bound, Connected, SocketAddrExt, SocketKind, SocketState,
        dns::{remote_getaddrinfo, unix::getaddrinfo as getaddrinfo_lib},
        ops::{ConnectResult, connect_common, connect_outgoing_common, nop_connect_fn},
    },
};
use mirrord_protocol::{
    file::{OpenFileResponse, OpenOptionsInternal, ReadFileResponse},
    outgoing::SocketAddress,
};
use nix::{
    errno::Errno,
    sys::socket::{SockaddrLike, SockaddrStorage},
};
use socket2::SockAddr;
#[cfg(debug_assertions)]
use tracing::Level;
use tracing::{error, trace, warn};

use super::{hooks::*, *};
use crate::file::{self, OPEN_FILES};

/// Hostname initialized from the agent with [`gethostname`].
pub(crate) static HOSTNAME: OnceLock<CString> = OnceLock::new();

/// Globals used by `gethostbyname`.
static mut GETHOSTBYNAME_HOSTNAME: Option<CString> = None;
static mut GETHOSTBYNAME_ALIASES_STR: Option<Vec<CString>> = None;

/// **Safety**:
/// There is a potential UB trigger here, as [`libc::hostent`] uses this as an `*mut _`, while we
/// have `*const _`. As this is being filled to fulfill the contract of a deprecated function, I
/// (alex) don't think we're going to hit this issue ever.
static mut GETHOSTBYNAME_ALIASES_PTR: Option<Vec<*const i8>> = None;
static mut GETHOSTBYNAME_ADDRESSES_VAL: Option<Vec<[u8; 4]>> = None;
static mut GETHOSTBYNAME_ADDRESSES_PTR: Option<Vec<*mut u8>> = None;

/// Global static that the user will receive when calling [`gethostbyname`].
///
/// **Safety**:
/// Even though we fill it with some `*const _` while it expects `*mut _`, it shouldn't be a problem
/// as the user will most likely be doing a deep copy if they want to mess around with this struct.
static mut GETHOSTBYNAME_HOSTENT: hostent = hostent {
    h_name: ptr::null_mut(),
    h_aliases: ptr::null_mut(),
    h_addrtype: 0,
    h_length: 0,
    h_addr_list: ptr::null_mut(),
};

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret)]
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> Detour<RawFd> {
    let socket_kind = type_.try_into()?;

    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) || (domain == libc::AF_UNIX)) {
        Err(Bypass::Domain(domain))
    } else {
        Ok(())
    }?;

    if domain == libc::AF_INET6 && crate::setup().layer_config().feature.network.ipv6.not() {
        return Detour::Error(HookError::SocketUnsuportedIpv6);
    }

    let socket_result = unsafe { FN_SOCKET(domain, type_, protocol) };

    let socket_fd = if socket_result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(socket_result)
    }?;

    let new_socket = UserSocket::new(domain, type_, protocol, Default::default(), socket_kind);

    SOCKETS.lock()?.insert(socket_fd, Arc::new(new_socket));

    Detour::Success(socket_fd)
}

/// Tries to bind the given socket to the requested address, with fallbacks.
///
/// Tried addresses, in order:
/// 1. Requested ip + requested port.
/// 2. Requested ip + port 0, if `can_use_random_port`.
/// 3. IPv4 localhost / IPv6 unspecified + requested port, if the requested ip is not
///    loopback/unspecified.
/// 3. IPv4 localhost / IPv6 unspecified + port 0, if the requested ip is not loopback/unspecified,
///    and `can_use_random_port`.
///
/// # Rationale
///
/// ## Why fallback to port 0?
///
/// Because, most of the time, the app will want to listen on a restricted low port, like 80 or 443.
/// This is fine when the app is deployed in a container, but will most likely fail if we try to do
/// it locally on the user machine.
///
/// ## Why fallback to localhost/unspecified?
///
/// The app might be attempting to bind to an address fetched from the remote target.
/// In other words, to an interface of the target pod.
///
/// ## Why fallback to localhost for IPv4 and unspecified for IPv6?
///
/// <https://www.visitourchina.com/special/china-great-wall/stories-and-legends-03.html>
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
fn bind_similar_address(
    sockfd: c_int,
    requested: SocketAddr,
    can_use_random_port: bool,
) -> Detour<()> {
    let address = SockaddrStorage::from(requested);
    if nix::sys::socket::bind(sockfd, &address).is_ok() {
        return Detour::Success(());
    };

    if can_use_random_port {
        let address = SockaddrStorage::from(SocketAddr::new(requested.ip(), 0));
        if nix::sys::socket::bind(sockfd, &address).is_ok() {
            return Detour::Success(());
        };
    }

    if requested.ip().is_loopback() || requested.ip().is_unspecified() {
        return Detour::Error(io::Error::last_os_error().into());
    }

    let new_ip = if requested.ip().is_ipv4() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    };
    let address = SockaddrStorage::from(SocketAddr::new(new_ip, requested.port()));
    if nix::sys::socket::bind(sockfd, &address).is_ok() {
        return Detour::Success(());
    }

    if can_use_random_port {
        let address = SockaddrStorage::from(SocketAddr::new(new_ip, 0));
        if nix::sys::socket::bind(sockfd, &address).is_ok() {
            return Detour::Success(());
        }
    }

    Detour::Error(io::Error::last_os_error().into())
}

/// Checks if given TCP port needs to be ignored based on ports logic
fn is_ignored_tcp_port(addr: &SocketAddr, config: &IncomingConfig) -> bool {
    let mapped_port = crate::setup()
        .incoming_config()
        .port_mapping
        .get_by_left(&addr.port())
        .copied()
        .unwrap_or_else(|| addr.port());

    let have_whitelist_and_port_is_not_whitelisted = config
        .ports
        .as_ref()
        .is_some_and(|ports| ports.contains(&mapped_port).not());

    is_ignored_port(addr) || have_whitelist_and_port_is_not_whitelisted
}

/// If the socket is not found in [`SOCKETS`], bypass.
/// Otherwise, if it's not an ignored port, bind (possibly with a fallback to random port) and
/// update socket state in [`SOCKETS`]. If it's an ignored port, remove the socket from [`SOCKETS`].
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret, skip(raw_address)
)]
pub(super) fn bind(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<i32> {
    let requested_address = SocketAddr::try_from_raw(raw_address, address_length)?;
    let requested_port = requested_address.port();
    let incoming_config = crate::setup().incoming_config();
    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| {
                if !matches!(socket.state, SocketState::Initialized) {
                    Err(Bypass::InvalidState(sockfd))
                } else {
                    Ok(socket)
                }
            })?
    };

    // Check that the domain matches the requested address.
    let domain_valid = match socket.domain {
        libc::AF_INET => requested_address.is_ipv4(),
        libc::AF_INET6 => requested_address.is_ipv6(),
        _ => false,
    };
    if !domain_valid {
        Err(HookError::InvalidBindAddressForDomain)?;
    }

    // Check if the user's requested address isn't already in use, even though it's not actually
    // bound, as we bind to a different address, but if we don't check for this then we're
    // changing normal socket behavior (see issue #1123).
    // We check that port isn't 0 because if it's port 0 it can't really conflict.
    if requested_address.port() != 0
        && SOCKETS
            .lock()?
            .iter()
            .any(|(_, socket)| match &socket.state {
                SocketState::Initialized | SocketState::Connected(_) => false,
                SocketState::Bound { bound, .. } | SocketState::Listening(bound) => {
                    bound.requested_address == requested_address
                }
            })
    {
        Err(HookError::AddressAlreadyBound(requested_address))?;
    }

    let listen_port = incoming_config
        .listen_ports
        .get_by_left(&requested_address.port())
        .copied();

    // we don't use `is_localhost` here since unspecified means to listen
    // on all IPs.
    let will_not_trigger_subscription = (incoming_config.ignore_localhost
        && requested_address.ip().is_loopback())
        || ((matches!(socket.kind, SocketKind::Tcp(_)))
            && is_ignored_tcp_port(&requested_address, incoming_config)
            || crate::setup().is_debugger_port(&requested_address)
            || incoming_config.ignore_ports.contains(&requested_port));

    if will_not_trigger_subscription && listen_port.is_none() {
        return Detour::Bypass(Bypass::IgnoredInIncoming(requested_address));
    }

    #[cfg(target_os = "macos")]
    {
        let experimental = crate::setup().experimental();
        if experimental.disable_reuseaddr {
            let fd = unsafe { BorrowedFd::borrow_raw(sockfd) };
            if let Err(e) =
                nix::sys::socket::setsockopt(&fd, nix::sys::socket::sockopt::ReuseAddr, &false)
            {
                tracing::debug!(?e, "Failed to set SO_REUSEADDR to false");
            }
            if let Err(e) =
                nix::sys::socket::setsockopt(&fd, nix::sys::socket::sockopt::ReusePort, &false)
            {
                tracing::debug!(?e, "Failed to set SE_REUSEPORT to false");
            }
        }
    }
    // Try to bind a port from listen ports, if no configuration
    // try to bind the requested port, if not available get a random port
    // if there's configuration and binding fails with the requested port
    // we return address not available and not fallback to a random port.
    if let Some(port) = listen_port {
        // Listen port was specified. If we fail to bind, we should fail the whole operation.
        let mut address = requested_address;
        address.set_port(port);
        bind_similar_address(sockfd, address, false)
    } else {
        // Listen port was not specified. If we fail to bind, it's ok to fall back to a random port.
        bind_similar_address(sockfd, requested_address, true)
    }?;

    // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
    // connect to.
    let Ok(address) = nix::sys::socket::getsockname::<SockaddrStorage>(sockfd) else {
        let error = io::Error::last_os_error();
        tracing::error!(
            sockfd,
            %error,
            "Failed to retrieve socket local address after intercepted bind."
        );
        return Detour::Error(error.into());
    };
    let address: SocketAddr = if let Some(ipv4) = address.as_sockaddr_in() {
        SocketAddrV4::from(*ipv4).into()
    } else if let Some(ipv6) = address.as_sockaddr_in6() {
        SocketAddrV6::from(*ipv6).into()
    } else {
        tracing::error!(
            %address,
            sockfd,
            "Failed to retrieve socket local address after intercepted bind. \
            Should be an IPv4 or IPv6 address.",
        );
        return Detour::Bypass(Bypass::AddressConversion);
    };

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound {
        bound: Bound {
            requested_address,
            address,
        },
        is_only_bound: will_not_trigger_subscription,
    };

    SOCKETS.lock()?.insert(sockfd, socket);

    // node reads errno to check if bind was successful and doesn't care about the return value
    // (???)
    Errno::set_raw(0);
    Detour::Success(0)
}

/// Subscribe to the agent on the real port. Messages received from the agent on the real port will
/// later be routed to the fake local port.
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret)]
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Detour<i32> {
    let Some(mut socket) = SOCKETS.lock()?.remove(&sockfd) else {
        return Detour::Bypass(Bypass::LocalFdNotFound(sockfd));
    };

    let setup = crate::setup();

    if matches!(setup.incoming_config().mode, IncomingMode::Off) {
        return Detour::Bypass(Bypass::DisabledIncoming);
    }

    if setup.targetless() {
        warn!(
            "Listening while running targetless. A targetless agent is not exposed by \
        any service. Therefore, letting this port bind happen locally instead of on the \
        cluster.",
        );
        return Detour::Bypass(Bypass::BindWhenTargetless);
    }

    match socket.state {
        SocketState::Bound {
            bound: Bound {
                requested_address,
                address,
            },
            is_only_bound,
        } if is_only_bound.not() => {
            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
                let error = io::Error::last_os_error();

                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);

                Err(error)?
            }

            let mapped_port = setup
                .incoming_config()
                .port_mapping
                .get_by_left(&requested_address.port())
                .copied()
                .unwrap_or_else(|| requested_address.port());

            make_proxy_request_with_response(PortSubscribe {
                listening_on: address,
                subscription: setup.incoming_mode().subscription(mapped_port),
            })??;

            // this log message is expected by some E2E tests
            tracing::debug!("daemon subscribed port {}", requested_address.port());

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(Bound {
                requested_address,
                address,
            });

            SOCKETS.lock()?.insert(sockfd, socket);

            Detour::Success(listen_result)
        }
        SocketState::Listening(_) => {
            tracing::debug!("second listen called");
            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };

            SOCKETS.lock()?.insert(sockfd, socket);

            if listen_result != 0 {
                let error = io::Error::last_os_error();
                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);
                Err(error)?
            }

            Detour::Success(listen_result)
        }
        SocketState::Bound { .. } | SocketState::Initialized | SocketState::Connected(_) => {
            Detour::Bypass(Bypass::InvalidState(sockfd))
        }
    }
}

/// Implementation of BSD `connectx(2)` detour.
/// * If source address is present in `endpoints`, call [`bind`].
/// * Call [`connect`] with the given destination address.
/// * On success `connectx` is **supposed** to update `len` with the data length enqueued in the
///   socket's send buffer copied from `iov`. For simplicity, we always set `len` to 0 on success to
///   inform the caller that no data have been enqueued. `flags` is ignored for the same reason.
/// * `sae_srcif`, source interface index, is an optional field of `sa_endpoints_t`. When specified,
///   `connectx` chooses a source address on the interface. This is not supported by this detour.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret)]
pub(super) fn connectx(
    socket: RawFd,
    endpoints: *const sa_endpoints_t,
    associd: sae_associd_t,
    flags: c_uint,
    iov: *const iovec,
    iovcnt: c_uint,
    len: *mut size_t,
    connid: *mut sae_connid_t,
) -> Detour<ConnectResult> {
    if endpoints.is_null() {
        return Detour::Error(HookError::NullPointer);
    }

    // The parameter associd is reserved for future use, and must always be set to SAE_ASSOCID_ANY.
    if associd != SAE_ASSOCID_ANY {
        warn!("associd is not SAE_ASSOCID_ANY.");
    }
    // The parameter connid is also reserved for future use and should be set to NULL.
    if !connid.is_null() {
        warn!("connid is not null.");
    }

    let eps = unsafe {
        let eps = *endpoints;

        // Destination address must be specified.
        if eps.sae_dstaddr.is_null() {
            warn!("destination address is null");
            return Detour::Bypass(Bypass::InvalidArgValue);
        }

        eps
    };

    // Bind with source address if given.
    if !eps.sae_srcaddr.is_null() {
        bind(socket, eps.sae_srcaddr, eps.sae_srcaddrlen)?;
    }

    // Explicitly set `len` to 0.
    if !len.is_null() {
        unsafe {
            *len = 0;
        }
    }

    // Connect to the destination address.
    connect(socket, eps.sae_dstaddr, eps.sae_dstaddrlen)
}

/// Common logic between Tcp/Udp `connect`, when used for the outgoing traffic feature.
///
/// Sends a hook message that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request
/// interception procedure.
/// This returns errno so we can restore the correct errno in case result is -1 (until we get
/// back to the hook we might call functions that will corrupt errno)
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn connect_outgoing<const CALL_CONNECT: bool>(
    sockfd: RawFd,
    remote_address: SockAddr,
    user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
) -> Detour<ConnectResult> {
    let connect_fn = |layer_address: SockAddr| -> ConnectResult {
        if CALL_CONNECT {
            unsafe { FN_CONNECT(sockfd, layer_address.as_ptr(), layer_address.len()) }.into()
        } else {
            nop_connect_fn(layer_address)
        }
    };

    connect_outgoing_common(
        sockfd,
        remote_address,
        user_socket_info,
        protocol,
        connect_fn,
    )
    .into()
}

/// Handles 3 different cases, depending on if the outgoing traffic feature is enabled or not:
///
/// 1. Outgoing traffic is **disabled**: this just becomes a normal `libc::connect` call, removing
/// the socket from our list of managed sockets.
///
/// 2. Outgoing traffic is **enabled** and `socket.state` is `Initialized`: sends a hook message
/// that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request interception procedure.
///
/// 3. `sockt.state` is `Bound`: part of the tcp mirror feature.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(raw_address))]
pub(super) fn connect(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<ConnectResult> {
    let remote_address = SockAddr::try_from_raw(raw_address, address_length)?;
    let connect_fn = |layer_address: SockAddr| -> ConnectResult {
        unsafe { FN_CONNECT(sockfd, layer_address.as_ptr(), layer_address.len()) }.into()
    };
    trace!("in connect {:#?}", SOCKETS);

    connect_common(sockfd, remote_address, connect_fn).into()
}

/// For IPv6 server / IPv4 client connections, translate IPv4
/// addresses to IPv4-mapped-IPv6 addresses, inline with what happens
/// without mirrord.
///
/// For IPv4 server / IPv6 client, we map the peer address to
/// localhost, since we cannot map IPv6 to IPv4. Without mirrord, IPv4
/// servers cannot accept IPv6 connections anyway, so this is fine.
fn map_ipv64(domain: c_int, ip: SocketAddr) -> SocketAddr {
    match (domain, ip) {
        (libc::AF_INET, SocketAddr::V4(..)) | (libc::AF_INET6, SocketAddr::V6(..)) => ip,
        (libc::AF_INET6, SocketAddr::V4(v4)) => {
            SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port())
        }

        (libc::AF_INET, SocketAddr::V6(v6)) => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), v6.port())
        }

        (domain, _) => {
            // Something has gone *very* wrong
            graceful_exit!("IP socket domain was {domain}, which is neither AF_INET nor AF_INET6");
        }
    }
}

/// Thin wrapper around [`map_ipv64`] to bypass non-IP [`SocketAddr`]s
/// and do return type mapping.
fn map_socketaddress_ipv64(domain: c_int, addr: SocketAddress) -> SocketAddress {
    // Bypass any non-IP stuff
    let SocketAddress::Ip(ip) = addr else {
        return addr;
    };
    map_ipv64(domain, ip).into()
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
#[mirrord_layer_macro::instrument(level = Level::TRACE, skip(address, address_len))]
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let remote_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Detour::Success(map_socketaddress_ipv64(
                    socket.domain,
                    connected.remote_address.clone(),
                )),
                SocketState::Bound { .. }
                | SocketState::Initialized
                | SocketState::Listening(_) => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    trace!("getpeername -> remote_address {:#?}", remote_address);

    fill_address(address, address_len, remote_address.try_into()?)
}

/// Resolve the fake local address to the real local address.
///
/// ## Port `0` special
///
/// When [`libc::bind`]ing on port `0`, we change the port to be the actual bound port, as this is
/// consistent behavior with libc.
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret, skip(address, address_len))]
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let socket = SOCKETS
        .lock()?
        .get(&sockfd)
        .bypass(Bypass::LocalFdNotFound(sockfd))?
        .clone();
    let local_address: SockAddr = match &socket.state {
        SocketState::Connected(Connected {
            local_address: Some(addr),
            ..
        }) => map_socketaddress_ipv64(socket.domain, addr.clone()).try_into()?,
        SocketState::Connected(Connected {
            connection_id: Some(id),
            ..
        }) => {
            let response =
                make_proxy_request_with_response(OutgoingConnMetadataRequest { conn_id: *id })??;
            map_ipv64(socket.domain, response.in_cluster_address).into()
        }
        SocketState::Bound {
            bound: Bound {
                requested_address,
                address,
            },
            ..
        }
        | SocketState::Listening(Bound {
            requested_address,
            address,
        }) => {
            if requested_address.port() == 0 {
                SocketAddr::new(requested_address.ip(), address.port()).into()
            } else {
                (*requested_address).into()
            }
        }

        SocketState::Initialized | SocketState::Connected(_) => {
            return Detour::Bypass(Bypass::InvalidState(sockfd));
        }
    };

    trace!("getsockname -> local_address {:#?}", local_address);

    fill_address(address, address_len, local_address)
}

/// When the fd is "ours", we accept and use [`ConnMetadataRequest`] to retrieve peer address from
/// the internal proxy.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(address, address_len))]
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> Detour<RawFd> {
    let (domain, protocol, type_, port, listener_address) = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(Bound {
                    requested_address,
                    address,
                }) => Detour::Success((
                    socket.domain,
                    socket.protocol,
                    socket.type_,
                    requested_address.port(),
                    *address,
                )),
                SocketState::Bound { .. }
                | SocketState::Initialized
                | SocketState::Connected(_) => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    let peer_address = {
        let stream = unsafe { TcpStream::from_raw_fd(new_fd) };
        let peer_address = stream.peer_addr();
        let _fd = stream.into_raw_fd();
        peer_address?
    };

    let ConnMetadataResponse {
        remote_source,
        local_address,
    } = make_proxy_request_with_response(ConnMetadataRequest {
        listener_address,
        peer_address,
    })?;

    let state = SocketState::Connected(Connected {
        connection_id: None,
        remote_address: remote_source.into(),
        local_address: Some(SocketAddr::new(local_address, port).into()),
        layer_address: None,
    });

    let new_socket = UserSocket::new(domain, type_, protocol, state, type_.try_into()?);

    fill_address(address, address_len, remote_source.into())?;

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Detour::Success(new_fd)
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<(), HookError> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup::<true>(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

/// Managed part of our [`dup_detour`], that clones the `Arc<T>` thing we have keyed by `fd`
/// ([`UserSocket`], or [`file::ops::RemoteFile`]).
///
/// - `SWITCH_MAP`:
///
/// Indicates that we're switching the `fd` from [`SOCKETS`] to [`OPEN_FILES`] (or vice-versa).
///
/// We need this to properly handle some cases in [`fcntl`], [`dup2_detour`], and [`dup3_detour`].
/// Extra relevant for node on macos.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn dup<const SWITCH_MAP: bool>(fd: c_int, dup_fd: i32) -> Result<(), HookError> {
    let mut sockets = SOCKETS.lock()?;
    if let Some(socket) = sockets.get(&fd).cloned() {
        sockets.insert(dup_fd as RawFd, socket);

        if SWITCH_MAP {
            OPEN_FILES.lock()?.remove(&dup_fd);
        }

        return Ok(());
    }

    let mut open_files = OPEN_FILES.lock()?;
    if let Some(file) = open_files.get(&fd) {
        let cloned_file = file.clone();
        open_files.insert(dup_fd as RawFd, cloned_file);

        if SWITCH_MAP {
            sockets.remove(&dup_fd);
        }
    }

    Ok(())
}

/// Retrieves the result of calling `getaddrinfo` from a remote host (resolves remote DNS),
/// converting the result into a `Box` allocated raw pointer of `libc::addrinfo` (which is basically
/// a linked list of such type).
///
/// Even though individual parts of the received list may contain an error, this function will
/// still work fine, as it filters out such errors and returns a null pointer in this case.
///
/// # Protocol
///
/// `-layer` sends a request to `-agent` asking for the `-agent`'s list of `addrinfo`s (remote call
/// for the equivalent of this function).
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn getaddrinfo(
    rawish_node: Option<&CStr>,
    rawish_service: Option<&CStr>,
    raw_hints: Option<&libc::addrinfo>,
) -> Detour<*mut libc::addrinfo> {
    getaddrinfo_lib(rawish_node, rawish_service, raw_hints)
}

/// Retrieves the `hostname` from the agent's `/etc/hostname` to be used by [`gethostname`]
fn remote_hostname_string() -> Detour<CString> {
    if crate::setup().local_hostname() {
        Detour::Bypass(Bypass::LocalHostname)?;
    }

    let hostname_path = PathBuf::from("/etc/hostname");

    let OpenFileResponse { fd } = file::ops::RemoteFile::remote_open(
        hostname_path,
        OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    )?;

    let ReadFileResponse { bytes, read_amount } = file::ops::RemoteFile::remote_read(fd, 256)?;

    let _ = file::ops::RemoteFile::remote_close(fd).inspect_err(|fail| {
        trace!("Leaking remote file fd (should be harmless) due to {fail:#?}!")
    });

    CString::new(
        bytes
            .into_vec()
            .into_iter()
            .take(read_amount as usize - 1)
            .collect::<Vec<_>>(),
    )
    .map(Detour::Success)?
}

/// Resolves a hostname and set result to static global like the original `gethostbyname` does.
///
/// Used by erlang/elixir to resolve DNS.
///
/// **Safety**:
/// See the [`GETHOSTBYNAME_ALIASES_PTR`] docs. If you see this function being called and some weird
/// issue is going on, assume that you might've triggered the UB.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn gethostbyname(raw_name: Option<&CStr>) -> Detour<*mut hostent> {
    let name: String = raw_name
        .bypass(Bypass::NullNode)?
        .to_str()
        .map_err(|fail| {
            warn!("Failed converting `name` from `CStr` with {:#?}", fail);

            Bypass::CStrConversion
        })?
        .into();

    crate::setup().dns_selector().check_query(&name, 0)?;

    let hosts_and_ips = remote_getaddrinfo(name.clone(), 0, 0, 0, 0, 0)?;

    // We could `unwrap` here, as this would have failed on the previous conversion.
    let host_name = CString::new(name)?;

    if hosts_and_ips.is_empty() {
        Errno::set_raw(libc::EAI_NODATA);
        return Detour::Success(ptr::null_mut());
    }

    // We need `*mut _` at the end, so `ips` has to be `mut`.
    let (aliases, mut ips) = hosts_and_ips
        .into_iter()
        .filter_map(|(host, ip)| match ip {
            // Only care about ipv4s and hosts that exist.
            IpAddr::V4(ip) => {
                let c_host = CString::new(host).ok()?;
                Some((c_host, ip.octets()))
            }
            IpAddr::V6(ip) => {
                trace!("ipv6 received - ignoring - {ip:?}");
                None
            }
        })
        .fold(
            (Vec::default(), Vec::default()),
            |(mut aliases, mut ips), (host, octets)| {
                aliases.push(host);
                ips.push(octets);
                (aliases, ips)
            },
        );

    let mut aliases_ptrs: Vec<*const i8> = aliases
        .iter()
        .map(|alias| alias.as_ptr().cast())
        .collect::<Vec<_>>();
    let mut ips_ptrs = ips.iter_mut().map(|ip| ip.as_mut_ptr()).collect::<Vec<_>>();

    // Put a null ptr to signal end of the list.
    aliases_ptrs.push(ptr::null());
    ips_ptrs.push(ptr::null_mut());

    // Need long-lived values so we can take pointers to them.
    #[allow(static_mut_refs)]
    unsafe {
        GETHOSTBYNAME_HOSTNAME.replace(host_name);
        GETHOSTBYNAME_ALIASES_STR.replace(aliases);
        GETHOSTBYNAME_ALIASES_PTR.replace(aliases_ptrs);
        GETHOSTBYNAME_ADDRESSES_VAL.replace(ips);
        GETHOSTBYNAME_ADDRESSES_PTR.replace(ips_ptrs);

        // Fill the `*mut hostent` that the user will interact with.
        GETHOSTBYNAME_HOSTENT.h_name = GETHOSTBYNAME_HOSTNAME.as_ref().unwrap().as_ptr() as _;
        GETHOSTBYNAME_HOSTENT.h_length = 4;
        GETHOSTBYNAME_HOSTENT.h_addrtype = libc::AF_INET;
        GETHOSTBYNAME_HOSTENT.h_aliases = GETHOSTBYNAME_ALIASES_PTR.as_ref().unwrap().as_ptr() as _;
        GETHOSTBYNAME_HOSTENT.h_addr_list =
            GETHOSTBYNAME_ADDRESSES_PTR.as_ref().unwrap().as_ptr() as *mut *mut libc::c_char;
    }

    Detour::Success(std::ptr::addr_of!(GETHOSTBYNAME_HOSTENT) as _)
}

/// Resolve hostname from remote host with caching for the result
#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) fn gethostname() -> Detour<&'static CString> {
    HOSTNAME.get_or_detour_init(remote_hostname_string)
}

/// Retrieves the contents of remote's `/etc/resolv.conf`
#[cfg(target_os = "macos")]
#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) fn read_remote_resolv_conf() -> Detour<Vec<u8>> {
    let resolv_path = PathBuf::from("/etc/resolv.conf");

    let OpenFileResponse { fd } = file::ops::RemoteFile::remote_open(
        resolv_path,
        OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    )?;

    let ReadFileResponse { bytes, read_amount } = file::ops::RemoteFile::remote_read(fd, 4096)?;

    let _ = file::ops::RemoteFile::remote_close(fd).inspect_err(|fail| {
        trace!("Leaking remote file fd (should be harmless) due to {fail:#?}!")
    });

    Detour::Success(
        bytes
            .into_vec()
            .into_iter()
            .take(read_amount as usize)
            .collect::<Vec<_>>(),
    )
}

/// ## DNS resolution on port `53`
///
/// We handle UDP sockets by putting them in a sort of _semantically_ connected state, meaning that
/// we don't actually [`libc::connect`] `sockfd`, but we change the respective [`UserSocket`] to
/// [`SocketState::Connected`].
///
/// Here we check for this invariant, so if a user calls [`libc::recvmsg`] without previously
/// either connecting this `sockfd`, or calling [`libc::sendto`], we're going to panic.
///
/// It performs the [`fill_address`] requirement of [`libc::recvmsg`] with the correct remote
/// address (instead of using our interceptor address).
///
/// ## Any other port
///
/// When this function is called, we've already run [`libc::recvfrom`], and checked for a successful
/// result from it, so `raw_source` has been pre-filled for us, but it might contain the wrong
/// address, if the packet came from one of our modified (connected) sockets.
///
/// If the socket was bound to `0.0.0.0:{not 53}`, then `raw_source` here should be
/// `127.0.0.1:{not 53}`, keeping in mind that the sender socket remains with address
/// `0.0.0.0:{not 53}` (same behavior as not using mirrord).
///
/// When the socket is in a [`Connected`] state, we call [`fill_address`] with its `remote_address`,
/// instead of letting whatever came in `raw_source` through.
///
/// See [`send_to`] for more information.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(raw_source, source_length))]
pub(super) fn recv_from(
    sockfd: i32,
    recv_from_result: isize,
    raw_source: *mut sockaddr,
    source_length: *mut socklen_t,
) -> Detour<isize> {
    SOCKETS
        .lock()?
        .get(&sockfd)
        .and_then(|socket| match &socket.state {
            SocketState::Connected(Connected { remote_address, .. }) => {
                Some(remote_address.clone())
            }
            SocketState::Bound { .. } | SocketState::Initialized | SocketState::Listening(_) => {
                None
            }
        })
        .map(SocketAddress::try_into)?
        .map(|address| fill_address(raw_source, source_length, address))??;

    Errno::set_raw(0);
    Detour::Success(recv_from_result)
}

/// Helps manually resolving DNS on port `53` with UDP, see [`send_to`] and [`sendmsg`].
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn send_dns_patch(
    sockfd: RawFd,
    user_socket_info: Arc<UserSocket>,
    destination: SocketAddr,
) -> Detour<SockAddr> {
    let mut sockets = SOCKETS.lock()?;
    // We want to keep holding this socket.
    sockets.insert(sockfd, user_socket_info);

    // Sending a packet on port NOT 53.
    let destination = sockets
        .iter()
        .filter(|(_, socket)| socket.kind.is_udp())
        // Is the `destination` one of our sockets? If so, then we grab the actual address,
        // instead of the possibly fake address from mirrord.
        .find_map(|(_, receiver_socket)| match &receiver_socket.state {
            SocketState::Bound {
                bound:
                    Bound {
                        requested_address,
                        address,
                    },
                ..
            } => {
                // Special case for port `0`, see `getsockname`.
                if requested_address.port() == 0 {
                    (SocketAddr::new(requested_address.ip(), address.port()) == destination)
                        .then_some(*address)
                } else {
                    (*requested_address == destination).then_some(*address)
                }
            }
            SocketState::Connected(Connected {
                remote_address,
                layer_address,
                ..
            }) => {
                let remote_address: SocketAddr = remote_address.clone().try_into().ok()?;
                let layer_address: SocketAddr = layer_address.clone()?.try_into().ok()?;

                if remote_address == destination {
                    Some(layer_address)
                } else {
                    None
                }
            }
            SocketState::Listening(_) | SocketState::Initialized => None,
        })?;

    Detour::Success(SockAddr::from(destination))
}

/// ## DNS resolution on port `53`
///
/// There is a bit of trickery going on here, as this function first triggers a _semantical_
/// connection to an interceptor socket (we don't [`libc::connect`] this `sockfd`, just change the
/// [`UserSocket`] state), and only then calls the actual [`libc::sendto`] to send `raw_message` to
/// the interceptor address (instead of the `raw_destination`).
///
/// ## Any other port
///
/// When `destination.port != 53`, we search our [`SOCKETS`] to see if we're holding the
/// `destination` address, which would have the receiving socket bound to a different address than
/// what the user sees.
///
/// If we find `destination` as the `requested_address` of one of our [`Bound`] sockets, then we
/// [`libc::sendto`] to the bound `address`. A similar logic applies to a [`Connected`] socket.
///
/// ## Destination is `0.0.0.0:{not 53}`
///
/// No special care is taken here, sending a packet to this address behaves the same with or without
/// mirrord, which is: destination becomes `127.0.0.1:{not 53}`.
///
/// See [`recv_from`] for more information.
#[mirrord_layer_macro::instrument(
    level = "trace",
    ret,
    skip(raw_message, raw_destination, destination_length)
)]
pub(super) fn send_to(
    sockfd: RawFd,
    raw_message: *const c_void,
    message_length: usize,
    flags: i32,
    raw_destination: *const sockaddr,
    destination_length: socklen_t,
) -> Detour<isize> {
    let destination = SockAddr::try_from_raw(raw_destination, destination_length)?;
    trace!("destination {:?}", destination.as_socket());

    let user_socket_info = SOCKETS
        .lock()?
        .remove(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?;

    // we don't support unix sockets which don't use `connect`
    if (destination.is_unix() || user_socket_info.domain == AF_UNIX)
        && !matches!(user_socket_info.state, SocketState::Connected(_))
    {
        return Detour::Bypass(Bypass::Domain(AF_UNIX));
    }

    // Currently this flow only handles DNS resolution.
    // So here we have to check for 2 things:
    //
    // 1. Are we sending something port 53? Then we use mirrord flow;
    // 2. Is the destination a socket that we have bound? Then we send it to the real address that
    // we've bound the destination socket.
    //
    // If none of the above are true, then the destination is some real address outside our scope.
    let sent_result = if let Some(destination) = destination
        .as_socket()
        .filter(|destination| destination.port() != 53)
    {
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination)?;

        unsafe {
            FN_SEND_TO(
                sockfd,
                raw_message,
                message_length,
                flags,
                rawish_true_destination.as_ptr(),
                rawish_true_destination.len(),
            )
        }
    } else {
        connect_outgoing::<false>(
            sockfd,
            destination,
            user_socket_info,
            NetProtocol::Datagrams,
        )?;

        let layer_address: SockAddr = SOCKETS
            .lock()?
            .get(&sockfd)
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => connected.layer_address.clone(),
                SocketState::Initialized
                | SocketState::Listening(_)
                | SocketState::Bound { .. } => {
                    unreachable!()
                }
            })
            .map(SocketAddress::try_into)??;

        let raw_interceptor_address = layer_address.as_ptr();
        let raw_interceptor_length = layer_address.len();

        unsafe {
            FN_SEND_TO(
                sockfd,
                raw_message,
                message_length,
                flags,
                raw_interceptor_address,
                raw_interceptor_length,
            )
        }
    };

    Detour::Success(sent_result)
}

/// Same behavior as [`send_to`], the only difference is that here we deal with [`libc::msghdr`],
/// instead of directly with socket addresses.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(raw_message_header))]
pub(super) fn sendmsg(
    sockfd: RawFd,
    raw_message_header: *const libc::msghdr,
    flags: i32,
) -> Detour<isize> {
    // We have a destination, so apply our fake `connect` patch.
    let destination = (!unsafe { *raw_message_header }.msg_name.is_null()).then(|| {
        let raw_destination = unsafe { *raw_message_header }.msg_name as *const libc::sockaddr;
        let destination_length = unsafe { *raw_message_header }.msg_namelen;
        SockAddr::try_from_raw(raw_destination, destination_length)
    })??;

    trace!("destination {:?}", destination.as_socket());

    // send_dns_patch acquires lock, so don't hold it
    let user_socket_info = SOCKETS
        .lock()?
        .remove(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?;

    // we don't support unix sockets which don't use `connect`
    if (destination.is_unix() || user_socket_info.domain == AF_UNIX)
        && !matches!(user_socket_info.state, SocketState::Connected(_))
    {
        return Detour::Bypass(Bypass::Domain(AF_UNIX));
    }

    // Currently this flow only handles DNS resolution.
    // So here we have to check for 2 things:
    //
    // 1. Are we sending something port 53? Then we use mirrord flow;
    // 2. Is the destination a socket that we have bound? Then we send it to the real address that
    // we've bound the destination socket.
    //
    // If none of the above are true, then the destination is some real address outside our scope.
    let sent_result = if let Some(destination) = destination
        .as_socket()
        .filter(|destination| destination.port() != 53)
    {
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination)?;

        let mut true_message_header = Box::new(unsafe { *raw_message_header });

        unsafe {
            true_message_header
                .as_mut()
                .msg_name
                .copy_from_nonoverlapping(
                    rawish_true_destination.as_ptr() as *const _,
                    rawish_true_destination.len() as usize,
                )
        };
        true_message_header.as_mut().msg_namelen = rawish_true_destination.len();

        unsafe { FN_SENDMSG(sockfd, true_message_header.as_ref(), flags) }
    } else {
        connect_outgoing::<false>(
            sockfd,
            destination,
            user_socket_info,
            NetProtocol::Datagrams,
        )?;

        let layer_address: SockAddr = SOCKETS
            .lock()?
            .get(&sockfd)
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => connected.layer_address.clone(),
                SocketState::Initialized
                | SocketState::Listening(_)
                | SocketState::Bound { .. } => {
                    unreachable!()
                }
            })
            .map(SocketAddress::try_into)??;

        let raw_interceptor_address = layer_address.as_ptr() as *const _;
        let raw_interceptor_length = layer_address.len();
        let mut true_message_header = Box::new(unsafe { *raw_message_header });

        unsafe {
            true_message_header
                .as_mut()
                .msg_name
                .copy_from_nonoverlapping(raw_interceptor_address, raw_interceptor_length as usize)
        };
        true_message_header.as_mut().msg_namelen = raw_interceptor_length;

        unsafe { FN_SENDMSG(sockfd, true_message_header.as_ref(), flags) }
    };

    Detour::Success(sent_result)
}

/// helper to reconstruct a [`dns_resolver_t`] for [`remote_dns_configuration_copy`]
/// NOTE: do free any memory "leaked" in this function over at [`free_dns_resolver_t`]
#[cfg(target_os = "macos")]
fn create_dns_resolver_t(
    nameservers: impl IntoIterator<Item = socket2::SockAddr>,
    search: impl IntoIterator<Item = CString>,
    options: CString,
) -> Box<dns_resolver_t> {
    let nameserver: Vec<*mut libc::sockaddr> = nameservers
        .into_iter()
        // Remember to free in [`free_dns_resolver_t`]
        .map(|sockaddr| Box::into_raw(Box::new(sockaddr.as_storage())).cast())
        .collect();

    // Remember to free in [`free_dns_resolver_t`]
    let (nameserver, n_nameserver, _) = nameserver.into_raw_parts();

    let search: Vec<*mut i8> = search
        .into_iter()
        // Remember to free in [`free_dns_resolver_t`]
        .map(|sockaddr| sockaddr.into_raw().cast())
        .collect();

    // Remember to free in [`free_dns_resolver_t`]
    let (search, n_search, _) = search.into_raw_parts();

    // Remember to free in [`free_dns_resolver_t`]
    Box::new(dns_resolver_t {
        domain: std::ptr::null_mut(),
        n_nameserver: n_nameserver as i32,
        nameserver,
        port: 0,
        n_search: n_search as i32,
        search,
        n_sortaddr: 0,
        sortaddr: std::ptr::null_mut(),
        // Remember to free in [`free_dns_resolver_t`]
        options: options.into_raw(),
        timeout: 0,
        search_order: 0,
        if_index: 0,
        flags: 0,
        reach_flags: 2,
        reserved: [0; 5],
    })
}

/// This only performs free operations on values that are created in [`create_dns_resolver_t`]
#[cfg(target_os = "macos")]
#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) unsafe fn free_dns_resolver_t(resolver: *mut dns_resolver_t) {
    unsafe {
        let resolver = Box::from_raw(resolver);

        let nameservers = Vec::from_raw_parts(
            resolver.nameserver,
            resolver.n_nameserver as usize,
            resolver.n_nameserver as usize,
        );

        for nameserver in nameservers {
            let _ = Box::from_raw(nameserver);
        }

        let searchs = Vec::from_raw_parts(
            resolver.search,
            resolver.n_search as usize,
            resolver.n_search as usize,
        );

        for search in searchs {
            let _ = CString::from_raw(search);
        }

        let _ = CString::from_raw(resolver.options);
    }
}

/// reconstruct a macos specific [`dns_config_t`] api from parsing the `/etc/resolv.conf` file from
/// remote container and fill only the first resolver in the response
#[cfg(target_os = "macos")]
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn remote_dns_configuration_copy() -> Detour<*mut dns_config_t> {
    let remote = read_remote_resolv_conf()?;

    // TODO: possibly create a different error rather than the almost correct
    // [`HookError::DNSNoName`]
    let resolv_conf = resolv_conf::Config::parse(remote).map_err(|_error| HookError::DNSNoName)?;

    let options = CString::new(format!(
        "ndots:{ndots} attempts:{attempts}",
        ndots = resolv_conf.ndots,
        attempts = resolv_conf.attempts
    ))
    .unwrap_or_default();

    let search: Vec<_> = resolv_conf
        .get_search()
        .map(|search| {
            search
                .iter()
                .filter_map(|search| CString::new(search.as_str()).ok())
                .collect()
        })
        .unwrap_or_default();

    let nameservers = resolv_conf.nameservers.into_iter().map(|nameserver| {
        socket2::SockAddr::from(match nameserver {
            resolv_conf::ScopedIp::V4(addr) => std::net::SocketAddr::new(addr.into(), 53),
            resolv_conf::ScopedIp::V6(addr, _) => std::net::SocketAddr::new(addr.into(), 53),
        })
    });

    let resolver = Box::into_raw(Box::new(create_dns_resolver_t(
        nameservers,
        search,
        options,
    )));

    let config = Box::into_raw(Box::new(dns_config_t {
        n_resolver: 1,
        resolver: resolver as *mut *mut dns_resolver_t,
        n_scoped_resolver: 0,
        scoped_resolver: std::ptr::null_mut(),
        reserved: [0; 5],
    }));

    Detour::Success(config)
}

/// Calls [`libc::getifaddrs`] and removes IPv6 addresses from the list.
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret, err)]
pub(super) fn getifaddrs() -> HookResult<*mut libc::ifaddrs> {
    let mut original_head = std::ptr::null_mut();
    let result: i32 = unsafe { FN_GETIFADDRS(&mut original_head) };
    if result != 0 {
        Err(io::Error::from_raw_os_error(result))?;
    }

    // Count entries for new_list_start malloc
    let mut entry_count = 0;
    let mut count_head: *mut libc::ifaddrs = original_head;
    unsafe {
        while let Some(ifaddr) = count_head.as_mut() {
            entry_count += 1;
            count_head = ifaddr.ifa_next;
        }
    }

    // Allocate new list so we can safely free the original list later
    // Safety: We assume `libc::malloc` is the same allocator as the user's system.
    let new_list_start: *mut libc::ifaddrs = unsafe {
        libc::malloc((mem::size_of::<libc::ifaddrs>() as libc::size_t) * entry_count)
            as *mut libc::ifaddrs
    };
    // Address to place next new address
    let mut next_new: *mut libc::ifaddrs = new_list_start;
    // Currently inspected element of the original interface addresses list.
    let mut inspected: *mut libc::ifaddrs = original_head;
    // Previously element that was inserted into new list.
    let mut previous_new_entry: *mut libc::ifaddrs = std::ptr::null_mut();

    // Safety: we only dereference pointers received from libc. They should be nulls or point to
    // initialized memory.
    unsafe {
        while let Some(ifaddr) = inspected.as_mut() {
            let address = SockaddrStorage::from_raw(ifaddr.ifa_addr, None);

            match address.as_ref().and_then(SockaddrStorage::as_sockaddr_in6) {
                // If not ipv6, advance to the next interface address in the original list.
                // Move both `previous` and `inspected`.
                None => {
                    // Append the address to the new list by copying ifaddr to the list head, and
                    // setting next of previous then moving head
                    copy_nonoverlapping::<libc::ifaddrs>(inspected, next_new, 1);
                    if let Some(prev_addr) = previous_new_entry.as_mut() {
                        prev_addr.ifa_next = next_new;
                    }

                    previous_new_entry = next_new;
                    next_new = next_new.add(1);
                }
                Some(ipv6) => {
                    let interface_name = if ifaddr.ifa_name.is_null() {
                        None
                    } else {
                        Some(CStr::from_ptr(ifaddr.ifa_name))
                    };

                    tracing::info!(
                        ?interface_name,
                        interface_address = %ipv6,
                        "Removing IPv6 interface address from the list returned by libc `getifaddrs`",
                    );
                }
            }
            inspected = ifaddr.ifa_next;
            ifaddr.ifa_next = std::ptr::null_mut();
        }

        // Ensure that final element in new list doesn't point to another entry
        if let Some(prev_addr) = previous_new_entry.as_mut() {
            prev_addr.ifa_next = std::ptr::null_mut();
        }
    }

    // Free the original list
    unsafe { libc::freeifaddrs(original_head) };

    Ok(new_list_start)
}
