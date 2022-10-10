use alloc::ffi::CString;
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    ptr,
    sync::Arc,
};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::dns::LookupRecord;
use socket2::SockAddr;
use tokio::sync::oneshot;
use tracing::{debug, error, trace};
use trust_dns_resolver::config::Protocol;

use super::{hooks::*, *};
use crate::{
    common::{blocking_send_hook_message, GetAddrInfoHook, HookMessage},
    error::HookError,
    outgoing::{tcp::TcpOutgoing, udp::UdpOutgoing, Connect, MirrorAddress},
    tcp::{HookMessageTcp, Listen},
    ENABLED_TCP_OUTGOING, ENABLED_UDP_OUTGOING,
};

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
#[tracing::instrument(level = "trace")]
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> HookResult<RawFd> {
    let socket_kind = type_.try_into()?;

    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) || (domain == libc::AF_UNIX)) {
        Err(HookError::BypassedDomain(domain))
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

    let mut sockets = SOCKETS.lock()?;
    sockets.insert(socket_fd, Arc::new(new_socket));

    Ok(socket_fd)
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state.
#[tracing::instrument(level = "trace")]
pub(super) fn bind(sockfd: c_int, address: SockAddr) -> HookResult<()> {
    let requested_address = address.as_socket().ok_or(HookError::AddressConversion)?;

    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))
            .and_then(|socket| {
                if !matches!(socket.state, SocketState::Initialized) {
                    Err(HookError::SocketInvalidState(sockfd))
                } else {
                    Ok(socket)
                }
            })?
    };

    let requested_port = requested_address.port();

    if is_ignored_port(requested_port) {
        return Err(HookError::BypassedPort(requested_address.port()));
    }

    let unbound_address = match socket.domain {
        libc::AF_INET => Ok(SockAddr::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
        ))),
        libc::AF_INET6 => Ok(SockAddr::from(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            0,
        ))),
        invalid => Err(HookError::UnsupportedDomain(invalid)),
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
    .map(|(_, address)| address.as_socket())?
    .ok_or(HookError::AddressConversion)?;

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound {
        requested_port,
        address,
    });

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}

/// Subscribe to the agent on the real port. Messages received from the agent on the real port will
/// later be routed to the fake local port.
#[tracing::instrument(level = "trace")]
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> HookResult<()> {
    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))?
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

            blocking_send_hook_message(HookMessage::Tcp(HookMessageTcp::Listen(Listen {
                mirror_port: address.port(),
                requested_port,
                ipv6: address.is_ipv6(),
                fd: sockfd,
            })))?;

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(Bound {
                requested_port,
                address,
            });

            Ok(())
        }
        _ => Err(HookError::SocketInvalidState(sockfd)),
    }?;

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
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
fn connect_outgoing<const TYPE: ConnectType>(
    sockfd: RawFd,
    remote_address: SocketAddr,
    mut user_socket_info: Arc<UserSocket>,
) -> HookResult<i32> {
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
    let MirrorAddress(mirror_address) = mirror_rx.blocking_recv()??;

    let connect_to = SockAddr::from(mirror_address);

    // Connect to the interceptor socket that is listening.
    let connect_result = unsafe { FN_CONNECT(sockfd, connect_to.as_ptr(), connect_to.len()) };

    let err_code = errno::errno().0;
    if connect_result == -1 && err_code != libc::EINPROGRESS && err_code != libc::EINTR {
        error!(
            "connect -> Failed call to libc::connect with {:#?} errno is {:#?}",
            connect_result,
            errno::errno()
        );
        return Err(io::Error::last_os_error())?;
    }

    let connected = Connected {
        remote_address,
        mirror_address,
    };

    Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);
    SOCKETS.lock()?.insert(sockfd, user_socket_info);

    Ok(connect_result)
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
pub(super) fn connect(sockfd: RawFd, remote_address: SocketAddr) -> HookResult<i32> {
    let user_socket_info = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))?
    };

    let enabled_tcp_outgoing = ENABLED_TCP_OUTGOING
        .get()
        .copied()
        .expect("Should be set during initialization!");

    let enabled_udp_outgoing = ENABLED_UDP_OUTGOING
        .get()
        .copied()
        .expect("Should be set during initialization!");

    (!is_ignored_port(remote_address.port()))
        .then_some(())
        .ok_or_else(|| HookError::BypassedPort(remote_address.port()))?;

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
            Err(io::Error::last_os_error())?
        } else {
            Ok(result)
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
) -> HookResult<()> {
    let remote_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.remote_address),
                _ => Err(HookError::SocketInvalidState(sockfd)),
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
) -> HookResult<()> {
    trace!("getsockname -> sockfd {:#?}", sockfd);

    let local_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.mirror_address),
                SocketState::Bound(bound) => Ok(bound.address),
                SocketState::Listening(bound) => Ok(bound.address),
                _ => Err(HookError::SocketInvalidState(sockfd)),
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
) -> HookResult<RawFd> {
    let (local_address, domain, protocol, type_) = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(bound) => {
                    Ok((bound.address, socket.domain, socket.protocol, socket.type_))
                }
                _ => Err(HookError::SocketInvalidState(sockfd)),
            })?
    };

    let remote_address = {
        CONNECTION_QUEUE
            .lock()?
            .get(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))
            .map(|socket| socket.address)?
    };

    let new_socket = UserSocket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            mirror_address: local_address,
        }),
        kind: type_.try_into()?,
    };
    fill_address(address, address_len, remote_address)?;

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Ok(new_fd)
}

#[tracing::instrument(level = "trace")]
pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> HookResult<()> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

#[tracing::instrument(level = "trace")]
pub(super) fn dup(fd: c_int, dup_fd: i32) -> HookResult<()> {
    let dup_socket = SOCKETS
        .lock()?
        .get(&fd)
        .ok_or(HookError::LocalFDNotFound(fd))?
        .clone();

    SOCKETS.lock()?.insert(dup_fd as RawFd, dup_socket);

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
    node: Option<String>,
    service: Option<String>,
    protocol: Option<Protocol>,
    ai_protocol: i32,
    ai_socktype: i32,
) -> HookResult<*mut libc::addrinfo> {
    let (hook_channel_tx, hook_channel_rx) = oneshot::channel();
    let hook = GetAddrInfoHook {
        node,
        hook_channel_tx,
    };

    blocking_send_hook_message(HookMessage::GetAddrInfoHook(hook))?;

    let addr_info_list = hook_channel_rx.blocking_recv()??;

    // Convert `service` into a port.
    let service = service.map_or(0, |s| s.parse().unwrap_or_default());

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
        .reduce(|current, mut previous| {
            // Safety: These pointers were just allocated using `Box::new`, so they should be
            // fine regarding memory layout, and are not dangling.
            unsafe { (*previous).ai_next = current };
            previous
        })
        .ok_or(HookError::DNSNoName);

    debug!("getaddrinfo -> result {:#?}", result);

    result
}
