use std::{
    ffi::CString,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
    os::unix::{io::RawFd, prelude::FromRawFd},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use dns_lookup::AddrInfo;
use errno::{set_errno, Errno};
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use socket2::{Domain, SockAddr};
use tokio::{net::TcpStream, sync::oneshot};
use tracing::{debug, error, trace};

use super::{hooks::*, *};
use crate::{
    common::{blocking_send_hook_message, GetAddrInfoHook, HookMessage},
    error::LayerError,
    tcp::{
        outgoing::{Connect, OutgoingTraffic, UserStream},
        HookMessageTcp, Listen,
    },
};

// TODO(alex) [high] 2022-07-21: Separate sockets into 2 compartments, listen sockets go into a
// thread for listening, connect sockets into another thread. Try to get rid of the overall global
// theme around sockets.
// Worth it?

pub(crate) static IS_INTERNAL_CALL: AtomicBool = AtomicBool::new(false);

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
pub(super) fn socket(
    sockfd: RawFd,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> Result<RawFd, LayerError> {
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
pub(super) fn bind(sockfd: c_int, address: SocketAddr) -> Result<(), LayerError> {
    trace!("bind -> sockfd {:#?} | address {:#?}", sockfd, address);

    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| {
                if !matches!(socket.state, SocketState::Initialized) {
                    Err(LayerError::SocketInvalidState(sockfd))
                } else {
                    Ok(socket)
                }
            })?
    };

    (!is_ignored_port(address.port()))
        .then_some(())
        .ok_or_else(|| LayerError::BypassedPort(address.port()))?;

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound { address });

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}
/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Result<(), LayerError> {
    trace!("listen -> sockfd {:#?} | backlog {:#?}", sockfd, backlog);

    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))?
    };

    match &socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(*bound);

            let mut address = match socket.domain {
                libc::AF_INET => Ok(OsSocketAddr::from(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    0,
                ))),
                libc::AF_INET6 => Ok(OsSocketAddr::from(SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                    0,
                ))),
                invalid => Err(LayerError::UnsupportedDomain(invalid)),
            }?;

            let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };
            if bind_result != 0 {
                return Err(io::Error::from_raw_os_error(bind_result).into());
            }

            // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
            // connect to.
            let getsockname_result =
                unsafe { FN_GETSOCKNAME(sockfd, address.as_mut_ptr(), &mut address.len()) };
            if getsockname_result != 0 {
                return Err(io::Error::from_raw_os_error(getsockname_result).into());
            }

            let address = address.into_addr().ok_or(LayerError::AddressConversion)?;

            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
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
        _ => Err(LayerError::SocketInvalidState(sockfd)),
    }?;

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}

pub(super) fn connect(sockfd: RawFd, remote_address: SocketAddr) -> Result<(), LayerError> {
    trace!(
        "connect -> sockfd {:#?} | remote_address {:#?}",
        sockfd,
        remote_address
    );

    let mut user_socket_info = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))?
    };
    debug!("connect -> user_socket_info {:#?}", user_socket_info);

    // let user_socket = unsafe { socket2::Socket::from_raw_fd(sockfd) };
    // user_socket.connect(&SockAddr::from(remote_address))?;
    // user_socket.into_raw_fd();
    // TODO(alex) [high] 2022-07-27: Try it out by having a request in js, so our own app sends a
    // get request, instead of the curl. Think the main issue is that we're dealing with listen
    // rather than the actual connect part.

    if let SocketState::Initialized = user_socket_info.state {
        trace!(
            "connect -> SocketState::Initialized {:#?}",
            user_socket_info
        );
        // TODO(alex) [high] 2022-07-15: Implementation plan
        // 4. Calls to read on this socket in `agent` will contain data that we log, and then
        // write to `remote_address`;

        // We're creating an interceptor socket that talks with the user socket.
        let unbound_mirror_address = match Domain::from(user_socket_info.domain) {
            Domain::IPV4 => Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            Domain::IPV6 => Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)),
            _ => Err(LayerError::UnsupportedDomain(user_socket_info.domain)),
        }?;

        let (channel_tx, channel_rx) = oneshot::channel();

        // TODO(alex) [mid] 2022-07-26: Use TLS to make this `bind` call bypass our hook and use
        // the original libc function.
        // Actually, this is a more general idea, every hook should perform a similar check.

        IS_INTERNAL_CALL.swap(true, Ordering::Acquire);
        let mirror_listener = TcpListener::bind(unbound_mirror_address)?;

        let mirror_address = mirror_listener.local_addr()?;
        trace!("connect -> mirror_address {:#?}", mirror_address);

        let connect = Connect {
            mirror_listener,
            user_fd: sockfd,
            remote_address,
            channel_tx,
        };

        let connect_hook = OutgoingTraffic::Connect(connect);

        // TODO(alex) [high] 2202-07-20: Send this crap as blocking, try to bypass tokio.
        blocking_send_hook_message(HookMessage::OutgoingTraffic(connect_hook))?;
        channel_rx.blocking_recv()??;

        let connected = Connected {
            remote_address,
            mirror_address,
        };

        trace!("connect -> connected {:#?}", connected);

        // TODO(alex) [high] 2202-07-16:
        // // 1. We send a hook message to the agent;
        // 2. agent will create a `TcpListener` waiting for a connection on `intercept_address`;
        // 3. agent sends back to layer this address;
        // 4. layer connects to this intercepted address;
        //
        // Need a thread that will hold all these intercepted addresses in agent, so we can use
        // `set_namespace` in there.
        // let intercept_address = hook_channel_rx.recv()?;

        let raw_mirror_address = SockAddr::from(mirror_address);
        let result = unsafe {
            FN_CONNECT(
                sockfd,
                raw_mirror_address.as_ptr(),
                raw_mirror_address.len(),
            )
        };
        if result != 0 {
            return Err(LayerError::DNSNoName);
        }

        Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);
        let user_stream = TcpStream::from_std(unsafe { std::net::TcpStream::from_raw_fd(sockfd) })?;

        let user_stream = UserStream {
            stream: user_stream,
        };

        let user_stream_hook = OutgoingTraffic::UserStream(user_stream);

        blocking_send_hook_message(HookMessage::OutgoingTraffic(user_stream_hook))?;

        IS_INTERNAL_CALL.store(false, Ordering::Release);

        Ok::<(), LayerError>(())
    } else if let SocketState::Bound(bound) = user_socket_info.state {
        trace!("connect -> SocketState::Bound {:#?}", user_socket_info);
        let os_addr = OsSocketAddr::from(bound.address);
        let ret = unsafe { libc::bind(sockfd, os_addr.as_ptr(), os_addr.len()) };

        if ret != 0 {
            error!(
                "connect: failed to bind socket ret: {:?}, addr: {:?}, sockfd: {:?}",
                ret, os_addr, sockfd
            );

            return Err(LayerError::AddressConversion);
        } else {
            Ok(())
        }
    } else {
        let rawish_remote_address = SockAddr::from(remote_address);
        let result = unsafe {
            libc::connect(
                sockfd,
                rawish_remote_address.as_ptr(),
                rawish_remote_address.len(),
            )
        };
        if result != 0 {
            return Err(LayerError::DNSNoName);
        }

        Ok(())
    }?;

    Ok(())
}
/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Result<(), LayerError> {
    trace!("getpeername -> sockfd {:#?}", sockfd);

    let remote_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.remote_address),
                invalid => Err(LayerError::SocketInvalidState(sockfd)),
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
) -> Result<(), LayerError> {
    trace!("getsockname -> sockfd {:#?}", sockfd);

    let local_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.mirror_address),
                SocketState::Bound(bound) => Ok(bound.address),
                SocketState::Listening(bound) => Ok(bound.address),
                invalid => Err(LayerError::SocketInvalidState(sockfd)),
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
) -> Result<RawFd, LayerError> {
    let (local_address, domain, protocol, type_) = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(bound) => {
                    Ok((bound.address, socket.domain, socket.protocol, socket.type_))
                }
                invalid => Err(LayerError::SocketInvalidState(sockfd)),
            })?
    };

    let remote_address = {
        CONNECTION_QUEUE
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| Ok(socket.address))?
    };

    let new_socket = MirrorSocket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            mirror_address: local_address,
        }),
    };
    fill_address(address, address_len, remote_address);

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Ok(new_fd)
}

pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> c_int {
    if fcntl_fd == -1 {
        error!("fcntl failed");
        return fcntl_fd;
    }
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => {
            dup(orig_fd, fcntl_fd);
        }
        _ => (),
    }
    fcntl_fd
}

pub(super) fn dup(fd: c_int, dup_fd: i32) -> c_int {
    if dup_fd == -1 {
        error!("dup failed");
        return dup_fd;
    }
    let mut sockets = SOCKETS.lock().unwrap();
    if let Some(socket) = sockets.get(&fd) {
        let dup_socket = socket.clone();
        sockets.insert(dup_fd as RawFd, dup_socket);
    }
    dup_fd
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
) -> Result<*mut libc::addrinfo, LayerError> {
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
                socktype,
                protocol,
                address,
                sockaddr,
                canonname,
                flags,
            } = addr_info;

            let sockaddr = socket2::SockAddr::from(sockaddr);

            let canonname = canonname.map(CString::new).transpose().unwrap();
            let ai_canonname = canonname.map_or_else(ptr::null, |c_string| {
                let c_str = c_string.as_c_str();
                c_str.as_ptr()
            }) as *mut _;

            let c_addr_info = libc::addrinfo {
                ai_flags: flags,
                ai_family: address,
                ai_socktype: socktype,
                ai_protocol: protocol,
                ai_addrlen: sockaddr.len(),
                ai_addr: sockaddr.as_ptr() as *mut _,
                ai_canonname,
                ai_next: ptr::null_mut(),
            };

            c_addr_info
        })
        .rev()
        .map(Box::new)
        .map(Box::into_raw)
        .reduce(|current, mut previous| {
            // Safety: These pointers were just allocated using `Box::new`, so they should be fine
            // regarding memory layout, and are not dangling.
            unsafe { (*previous).ai_next = current };
            previous
        })
        .ok_or(LayerError::DNSNoName)
}
