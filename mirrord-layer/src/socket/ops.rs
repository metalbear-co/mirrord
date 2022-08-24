use std::{
    ffi::CString,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    ptr,
    sync::Arc,
};

use dns_lookup::AddrInfo;
use libc::{c_int, sockaddr, socklen_t};
use socket2::SockAddr;
use tokio::sync::oneshot;
use tracing::{debug, error, trace};

use super::{hooks::*, *};
use crate::{
    common::{blocking_send_hook_message, GetAddrInfoHook, HookMessage},
    error::{HookError, HookResult as Result},
    tcp::{HookMessageTcp, Listen},
};

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
pub(super) fn socket(sockfd: RawFd, domain: c_int, type_: c_int, protocol: c_int) -> Result<RawFd> {
    trace!("socket -> domain {:#?} | type:{:#?}", domain, type_);

    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        debug!("non Tcp socket domain:{:?}, type:{:?}", domain, type_);
    } else {
        let mut sockets = SOCKETS.lock()?;

        sockets.insert(
            sockfd,
            Arc::new(MirrorSocket {
                domain,
                type_,
                protocol,
                state: SocketState::default(),
            }),
        );
    }

    Ok(sockfd)
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state and don't call bind (will be called later). In any other case, we call
/// regular bind.
pub(super) fn bind(sockfd: c_int, address: SocketAddr) -> Result<()> {
    trace!("bind -> sockfd {:#?} | address {:#?}", sockfd, address);

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

    (!is_ignored_port(address.port()))
        .then_some(())
        .ok_or_else(|| HookError::BypassedPort(address.port()))?;

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound { address });

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Result<()> {
    debug!("listen -> sockfd {:#?} | backlog {:#?}", sockfd, backlog);

    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))?
    };

    match &socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(*bound);

            let address = match socket.domain {
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

            let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };
            if bind_result != 0 {
                error!(
                    "listen -> Failed `bind` sockfd {:#?} to address {:#?} with errno {:#?}!",
                    sockfd,
                    address,
                    errno::errno()
                );
                return Err(io::Error::from_raw_os_error(bind_result).into());
            }

            // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
            // connect to.
            let getsockname_result =
                unsafe { FN_GETSOCKNAME(sockfd, address.as_ptr() as *mut _, &mut address.len()) };
            if getsockname_result != 0 {
                error!("listen -> Failed `getsockname` sockfd {:#?}", sockfd);

                return Err(io::Error::from_raw_os_error(getsockname_result).into());
            }

            let address = address.as_socket().ok_or(HookError::AddressConversion)?;

            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);

                return Err(io::Error::from_raw_os_error(listen_result).into());
            }

            blocking_send_hook_message(HookMessage::Tcp(HookMessageTcp::Listen(Listen {
                fake_port: address.port(),
                real_port,
                ipv6: address.is_ipv6(),
                fd: sockfd,
            })))?;

            Ok(())
        }
        _ => Err(HookError::SocketInvalidState(sockfd)),
    }?;

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}

pub(super) fn connect(sockfd: RawFd, remote_address: SocketAddr) -> Result<()> {
    trace!(
        "connect -> sockfd {:#?} | remote_address {:#?}",
        sockfd,
        remote_address
    );

    let user_socket_info = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))?
    };
    debug!("connect -> user_socket_info {:#?}", user_socket_info);

    if let SocketState::Bound(bound) = user_socket_info.state {
        trace!("connect -> SocketState::Bound {:#?}", user_socket_info);

        let address = SockAddr::from(bound.address);
        let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };

        if bind_result != 0 {
            error!(
                "connect -> Failed to bind socket result {:?}, address: {:?}, sockfd: {:?}!",
                bind_result, address, sockfd
            );

            Err(io::Error::from_raw_os_error(bind_result))?
        } else {
            let rawish_remote_address = SockAddr::from(remote_address);
            let result = unsafe {
                FN_CONNECT(
                    sockfd,
                    rawish_remote_address.as_ptr(),
                    rawish_remote_address.len(),
                )
            };

            if result != 0 {
                Err(io::Error::from_raw_os_error(result))?
            } else {
                Ok::<_, HookError>(())
            }
        }
    } else {
        Err(HookError::SocketInvalidState(sockfd))
    }?;

    Ok(())
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Result<()> {
    trace!("getpeername -> sockfd {:#?}", sockfd);

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
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Result<()> {
    trace!("getsockname -> sockfd {:#?}", sockfd);

    let local_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(HookError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.local_address),
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
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> Result<RawFd> {
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

    let new_socket = MirrorSocket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            local_address,
        }),
    };
    fill_address(address, address_len, remote_address)?;

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Ok(new_fd)
}

pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<()> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

pub(super) fn dup(fd: c_int, dup_fd: i32) -> Result<()> {
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
pub(super) fn getaddrinfo(
    node: Option<String>,
    service: Option<String>,
    hints: Option<AddrInfoHint>,
) -> Result<*mut libc::addrinfo> {
    trace!(
        "getaddrinfo -> node {:#?} | service {:#?} | hints {:#?}",
        node,
        service,
        hints
    );

    let (hook_channel_tx, hook_channel_rx) = oneshot::channel();
    let hook = GetAddrInfoHook {
        node,
        service,
        hints,
        hook_channel_tx,
    };

    blocking_send_hook_message(HookMessage::GetAddrInfoHook(hook))?;

    let addr_info_list = hook_channel_rx.blocking_recv()??;

    addr_info_list
        .into_iter()
        .map(AddrInfo::from)
        .map(|addr_info| {
            let AddrInfo {
                socktype: ai_socktype,
                protocol: ai_protocol,
                address: ai_family,
                sockaddr,
                canonname,
                flags: ai_flags,
            } = addr_info;

            let rawish_sockaddr = socket2::SockAddr::from(sockaddr);
            let ai_addrlen = rawish_sockaddr.len();

            // Must outlive this function, as it is stored as a pointer in `libc::addrinfo`.
            let ai_addr = Box::into_raw(Box::new(unsafe { *rawish_sockaddr.as_ptr() }));

            let canonname = canonname.map(CString::new).transpose().unwrap();
            let ai_canonname = canonname.map_or_else(ptr::null, |c_string| {
                let c_str = c_string.as_c_str();
                c_str.as_ptr()
            }) as *mut _;

            libc::addrinfo {
                ai_flags,
                ai_family,
                ai_socktype,
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
        .ok_or(HookError::DNSNoName)
}
