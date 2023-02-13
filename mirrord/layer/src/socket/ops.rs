use alloc::ffi::CString;
use core::{ffi::CStr, mem};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    ptr,
    sync::{Arc, OnceLock},
};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::{dns::LookupRecord, file::OpenOptionsInternal};
use socket2::SockAddr;
use tokio::sync::oneshot;
use tracing::{debug, error, trace};

use super::{hooks::*, *};
use crate::{
    close_layer_fd,
    common::{blocking_send_hook_message, HookMessage},
    detour::{Detour, OnceLockExt, OptionExt},
    dns::GetAddrInfo,
    error::HookError,
    file::{self, OPEN_FILES},
    outgoing::{tcp::TcpOutgoing, udp::UdpOutgoing, Connect, RemoteConnection},
    port_debug_patch,
    tcp::{Listen, TcpIncoming},
    ENABLED_TCP_OUTGOING, ENABLED_UDP_OUTGOING,
};

/// Hostname initialized from the agent with [`gethostname`].
pub(crate) static HOSTNAME: OnceLock<CString> = OnceLock::new();

/// Helper struct for connect results where we want to hold the original errno
/// when result is -1 (error) because sometimes it's not a real error (EINPROGRESS/EINTR)
/// and the caller should have the original value.
/// In order to use this struct correctly, after calling connect convert the return value using
/// this `.into/.from` then the detour would call `.into` on the struct to convert to i32
/// and would set the according errno.
#[derive(Debug)]
pub(super) struct ConnectResult {
    result: i32,
    error: Option<errno::Errno>,
}

impl ConnectResult {
    pub(super) fn is_failure(&self) -> bool {
        matches!(self.error, Some(err) if err.0 != libc::EINTR && err.0 != libc::EINPROGRESS)
    }
}

impl From<i32> for ConnectResult {
    fn from(result: i32) -> Self {
        if result == -1 {
            ConnectResult {
                result,
                error: Some(errno::errno()),
            }
        } else {
            ConnectResult {
                result,
                error: None,
            }
        }
    }
}

impl From<ConnectResult> for i32 {
    fn from(connect_result: ConnectResult) -> Self {
        if let Some(error) = connect_result.error {
            errno::set_errno(error);
        }
        connect_result.result
    }
}

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
#[tracing::instrument(level = "trace")]
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> Detour<RawFd> {
    let socket_kind = type_.try_into()?;

    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) || (domain == libc::AF_UNIX)) {
        Err(Bypass::Domain(domain))
    } else {
        Ok(())
    }?;

    let socket_result = unsafe { FN_SOCKET(domain, type_, protocol) };

    let socket_fd = if socket_result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(socket_result)
    }?;

    let new_socket = UserSocket {
        domain,
        type_,
        protocol,
        state: SocketState::default(),
        kind: socket_kind,
    };

    SOCKETS.lock()?.insert(socket_fd, Arc::new(new_socket));

    Detour::Success(socket_fd)
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state.
#[tracing::instrument(level = "trace", skip(raw_address))]
pub(super) fn bind(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<i32> {
    let requested_address = SocketAddr::try_from_raw(raw_address, address_length)?;
    let requested_port = requested_address.port();

    if is_ignored_port(requested_address) || port_debug_patch(requested_address) {
        Err(Bypass::Port(requested_address.port()))?;
    }

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

    let unbound_address = match socket.domain {
        libc::AF_INET => Ok(SockAddr::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
        ))),
        libc::AF_INET6 => Ok(SockAddr::from(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            0,
        ))),
        invalid => Err(Bypass::Domain(invalid)),
    }?;

    trace!("bind -> unbound_address {:#?}", unbound_address);

    let bind_result = unsafe { FN_BIND(sockfd, unbound_address.as_ptr(), unbound_address.len()) };
    if bind_result != 0 {
        error!(
            "listen -> Failed `bind` sockfd {:#?} to address {:#?} with errno {:#?}!",
            sockfd,
            unbound_address,
            errno::errno()
        );
        return Err(io::Error::last_os_error())?;
    }

    // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
    // connect to.
    let address = unsafe {
        SockAddr::init(|storage, len| {
            if FN_GETSOCKNAME(sockfd, storage.cast(), len) == -1 {
                error!("bind -> Failed `getsockname` sockfd {:#?}", sockfd);

                Err(io::Error::last_os_error())?
            } else {
                Ok(())
            }
        })
    }
    .ok()
    .and_then(|(_, address)| address.as_socket())
    .bypass(Bypass::AddressConversion)?;

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound {
        requested_port,
        address,
    });

    SOCKETS.lock()?.insert(sockfd, socket);

    Detour::Success(bind_result)
}

/// Subscribe to the agent on the real port. Messages received from the agent on the real port will
/// later be routed to the fake local port.
#[tracing::instrument(level = "trace")]
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Detour<i32> {
    let mut socket: Arc<UserSocket> = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))?
    };

    match socket.state {
        SocketState::Bound(Bound {
            requested_port,
            address,
        }) => {
            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);

                return Err(io::Error::last_os_error())?;
            }

            blocking_send_hook_message(HookMessage::Tcp(TcpIncoming::Listen(Listen {
                mirror_port: address.port(),
                requested_port,
                ipv6: address.is_ipv6(),
                fd: sockfd,
            })))?;

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(Bound {
                requested_port,
                address,
            });

            SOCKETS.lock()?.insert(sockfd, socket);

            Detour::Success(listen_result)
        }
        _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
    }
}

// TODO(alex): Should be an enum, but to do so requires the `adt_const_params` feature, which also
// requires enabling `incomplete_features`.
type ConnectType = bool;
const TCP: ConnectType = false;
const UDP: ConnectType = !TCP;

/// Common logic between Tcp/Udp `connect`, when used for the outgoing traffic feature.
///
/// Sends a hook message that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request
/// interception procedure.
/// This returns errno so we can restore the correct errno in case result is -1 (until we get
/// back to the hook we might call functions that will corrupt errno)
#[tracing::instrument(level = "trace")]
fn connect_outgoing<const TYPE: ConnectType>(
    sockfd: RawFd,
    remote_address: SocketAddr,
    mut user_socket_info: Arc<UserSocket>,
) -> Detour<ConnectResult> {
    // Prepare this socket to be intercepted.
    let (mirror_tx, mirror_rx) = oneshot::channel();

    let connect = Connect {
        remote_address,
        channel_tx: mirror_tx,
    };

    let hook_message = match TYPE {
        TCP => {
            let connect_hook = TcpOutgoing::Connect(connect);
            HookMessage::TcpOutgoing(connect_hook)
        }
        UDP => {
            let connect_hook = UdpOutgoing::Connect(connect);
            HookMessage::UdpOutgoing(connect_hook)
        }
    };

    blocking_send_hook_message(hook_message)?;
    let RemoteConnection {
        mirror_address,
        local_address,
    } = mirror_rx.blocking_recv()??;

    let connect_to = SockAddr::from(mirror_address);

    // Connect to the interceptor socket that is listening.
    let connect_result: ConnectResult =
        unsafe { FN_CONNECT(sockfd, connect_to.as_ptr(), connect_to.len()) }.into();

    if connect_result.is_failure() {
        error!(
            "connect -> Failed call to libc::connect with {:#?}",
            connect_result,
        );
        return Err(io::Error::last_os_error())?;
    }

    let connected = Connected {
        remote_address,
        local_address,
    };

    Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);
    SOCKETS.lock()?.insert(sockfd, user_socket_info);

    Detour::Success(connect_result)
}

/// Handles 3 different cases, depending if the outgoing traffic feature is enabled or not:
///
/// 1. Outgoing traffic is **disabled**: this just becomes a normal `libc::connect` call, removing
/// the socket from our list of managed sockets.
///
/// 2. Outgoing traffic is **enabled** and `socket.state` is `Initialized`: sends a hook message
/// that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request interception procedure.
///
/// 3. `sockt.state` is `Bound`: part of the tcp mirror feature.
#[tracing::instrument(level = "trace")]
pub(super) fn connect(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<ConnectResult> {
    let remote_address = SocketAddr::try_from_raw(raw_address, address_length)?;

    if is_ignored_port(remote_address) || port_debug_patch(remote_address) {
        Err(Bypass::Port(remote_address.port()))?
    }

    let user_socket_info = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(Bypass::LocalFdNotFound(sockfd))?
    };

    let enabled_tcp_outgoing = ENABLED_TCP_OUTGOING
        .get()
        .copied()
        .expect("Should be set during initialization!");

    let enabled_udp_outgoing = ENABLED_UDP_OUTGOING
        .get()
        .copied()
        .expect("Should be set during initialization!");

    let raw_connect = |remote_address| {
        let rawish_remote_address = SockAddr::from(remote_address);
        let result = unsafe {
            FN_CONNECT(
                sockfd,
                rawish_remote_address.as_ptr(),
                rawish_remote_address.len(),
            )
        };

        if result != 0 {
            Detour::Error(Err(io::Error::last_os_error())?)?
        } else {
            Detour::Success(result.into())
        }
    };

    // if it's loopback, check if it's a port we're listening to and if so, just let it connect
    // locally.
    if remote_address.ip().is_loopback() && let Some(res) =
        SOCKETS.lock()?.values().find_map(|socket| {
            if let SocketState::Listening(Bound {
                requested_port,
                address,
            }) = socket.state
            {
                if requested_port == remote_address.port()
                    && socket.protocol == user_socket_info.protocol
                {
                    return Some(raw_connect(address));
                }
            }
            None
        }) {
        return res;
    };

    match user_socket_info.kind {
        SocketKind::Udp(_) if enabled_udp_outgoing => {
            connect_outgoing::<UDP>(sockfd, remote_address, user_socket_info)
        }
        SocketKind::Tcp(_) => match user_socket_info.state {
            SocketState::Initialized if enabled_tcp_outgoing => {
                connect_outgoing::<TCP>(sockfd, remote_address, user_socket_info)
            }
            SocketState::Bound(Bound { address, .. }) => {
                trace!("connect -> SocketState::Bound {:#?}", user_socket_info);

                let address = SockAddr::from(address);
                let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };

                if bind_result != 0 {
                    error!(
                    "connect -> Failed to bind socket result {:?}, address: {:?}, sockfd: {:?}!",
                    bind_result, address, sockfd
                );

                    Err(io::Error::last_os_error())?
                } else {
                    raw_connect(remote_address)
                }
            }
            _ => raw_connect(remote_address),
        },
        _ => raw_connect(remote_address),
    }
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
#[tracing::instrument(level = "trace", skip(address, address_len))]
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
                SocketState::Connected(connected) => Detour::Success(connected.remote_address),
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    debug!("getpeername -> remote_address {:#?}", remote_address);

    fill_address(address, address_len, remote_address)
}
/// Resolve the fake local address to the real local address.
#[tracing::instrument(level = "trace", skip(address, address_len))]
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let local_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Detour::Success(connected.local_address),
                SocketState::Bound(bound) => Detour::Success(bound.address),
                SocketState::Listening(bound) => Detour::Success(bound.address),
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    debug!("getsockname -> local_address {:#?}", local_address);

    fill_address(address, address_len, local_address)
}

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock This enables us to have a safe way to get "remote" information
/// (remote ip, port, etc).
#[tracing::instrument(level = "trace", skip(address, address_len))]
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> Detour<RawFd> {
    let (domain, protocol, type_) = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(_) => {
                    Detour::Success((socket.domain, socket.protocol, socket.type_))
                }
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    let (local_address, remote_address) = {
        CONNECTION_QUEUE
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .map(|socket| (socket.local_address, socket.remote_address))?
    };

    let new_socket = UserSocket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            local_address,
        }),
        kind: type_.try_into()?,
    };
    fill_address(address, address_len, remote_address)?;

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Detour::Success(new_fd)
}

#[tracing::instrument(level = "trace")]
pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<(), HookError> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

#[tracing::instrument(level = "trace")]
pub(super) fn dup(fd: c_int, dup_fd: i32) -> Result<(), HookError> {
    {
        let mut sockets = SOCKETS.lock()?;
        if let Some(socket) = sockets.get(&fd).cloned() {
            sockets.insert(dup_fd as RawFd, socket);
            return Ok(());
        }
    } // Drop sockets, free Mutex.

    let mut files = OPEN_FILES.lock()?;
    if let Some(file) = files.get(&fd).cloned() {
        files.insert(dup_fd as RawFd, file);
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
#[tracing::instrument(level = "trace")]
pub(super) fn getaddrinfo(
    rawish_node: Option<&CStr>,
    rawish_service: Option<&CStr>,
    raw_hints: Option<&libc::addrinfo>,
) -> Detour<*mut libc::addrinfo> {
    let node = rawish_node
        .bypass(Bypass::NullNode)?
        .to_str()
        .map_err(|fail| {
            warn!(
                "Failed converting `rawish_node` from `CStr` with {:#?}",
                fail
            );

            Bypass::CStrConversion
        })?
        .into();

    let service = rawish_service
        .map(CStr::to_str)
        .transpose()
        .map_err(|fail| {
            warn!(
                "Failed converting `raw_service` from `CStr` with {:#?}",
                fail
            );

            Bypass::CStrConversion
        })?
        .map(String::from);

    let raw_hints = raw_hints
        .cloned()
        .unwrap_or_else(|| unsafe { mem::zeroed() });

    // TODO(alex): Use more fields from `raw_hints` to respect the user's `getaddrinfo` call.
    let libc::addrinfo {
        ai_socktype,
        ai_protocol,
        ..
    } = raw_hints;

    let (hook_channel_tx, hook_channel_rx) = oneshot::channel();
    let hook = GetAddrInfo {
        node,
        hook_channel_tx,
    };

    blocking_send_hook_message(HookMessage::GetAddrinfo(hook))?;

    let addr_info_list = hook_channel_rx.blocking_recv()??;

    // Convert `service` into a port.
    let service = service.map_or(0, |s| s.parse().unwrap_or_default());
    let mut addrinfo_set = MANAGED_ADDRINFO.lock()?;
    // Only care about: `ai_family`, `ai_socktype`, `ai_protocol`.
    let result = addr_info_list
        .into_iter()
        .map(|LookupRecord { name, ip }| (name, SockAddr::from(SocketAddr::from((ip, service)))))
        .map(|(name, rawish_sock_addr)| {
            let ai_addrlen = rawish_sock_addr.len();
            let ai_family = rawish_sock_addr.family() as _;

            // Must outlive this function, as it is stored as a pointer in `libc::addrinfo`.
            let ai_addr = Box::into_raw(Box::new(unsafe { *rawish_sock_addr.as_ptr() }));
            let ai_canonname = CString::new(name).unwrap().into_raw();

            libc::addrinfo {
                ai_flags: 0,
                ai_family,
                ai_socktype,
                // TODO(alex): Don't just reuse whatever the user passed to us.
                ai_protocol,
                ai_addrlen,
                ai_addr,
                ai_canonname,
                ai_next: ptr::null_mut(),
            }
        })
        .rev()
        .map(Box::new)
        .map(Box::into_raw)
        .map(|raw| {
            addrinfo_set.insert(raw as usize);
            raw
        })
        .reduce(|current, mut previous| {
            // Safety: These pointers were just allocated using `Box::new`, so they should be
            // fine regarding memory layout, and are not dangling.
            unsafe { (*previous).ai_next = current };
            previous
        })
        .ok_or(HookError::DNSNoName)?;

    debug!("getaddrinfo -> result {:#?}", result);

    Detour::Success(result)
}

/// Retrieves the `hostname` from the agent's `/etc/hostname` to be used by [`gethostname`]
fn remote_hostname_string() -> Detour<CString> {
    let hostname_path = CString::new("/etc/hostname")?;

    let hostname_fd = file::ops::open(
        Some(&hostname_path),
        OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    )?;

    let hostname_file = file::ops::read(hostname_fd, 256)?;

    close_layer_fd(hostname_fd);

    CString::new(
        hostname_file
            .bytes
            .into_iter()
            .take(hostname_file.read_amount as usize - 1)
            .collect::<Vec<_>>(),
    )
    .map(Detour::Success)?
}

/// Resolve hostname from remote host with caching for the result
#[tracing::instrument(level = "trace")]
pub(super) fn gethostname() -> Detour<&'static CString> {
    HOSTNAME.get_or_detour_init(remote_hostname_string)
}
