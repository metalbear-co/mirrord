use std::{
    ffi::CString,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
    os::unix::{
        io::RawFd,
        prelude::{FromRawFd, IntoRawFd},
    },
    ptr,
    sync::Arc,
};

use dns_lookup::AddrInfo;
use errno::{errno, set_errno, Errno};
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use socket2::{Domain, SockAddr};
use tokio::sync::oneshot;
use tracing::{debug, error, trace, warn};

use super::*;
use crate::{
    common::{blocking_send_hook_message, GetAddrInfoHook, HookMessage},
    error::LayerError,
    tcp::{
        outgoing::{Connect, OutgoingTraffic},
        HookMessageTcp, Listen,
    },
    HOOK_SENDER,
};

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> RawFd {
    debug!("socket called domain:{:?}, type:{:?}", domain, type_);
    let fd = unsafe { libc::socket(domain, type_, protocol) };
    if fd == -1 {
        error!("socket failed");
        return fd;
    }
    // We don't handle non Tcpv4 sockets
    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        debug!("non Tcp socket domain:{:?}, type:{:?}", domain, type_);
        return fd;
    }
    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(
        fd,
        Arc::new(MirrorSocket {
            domain,
            type_,
            protocol,
            state: SocketState::default(),
        }),
    );
    fd
}
/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state and don't call bind (will be called later). In any other case, we call
/// regular bind.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn bind(sockfd: c_int, addr: *const sockaddr, addrlen: socklen_t) -> c_int {
    debug!("bind called sockfd: {:?}", sockfd);
    let mut socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.remove(&sockfd) {
            Some(socket) if !matches!(socket.state, SocketState::Initialized) => {
                error!("socket is in invalid state for bind {:?}", socket.state);
                return libc::EINVAL;
            }
            Some(socket) => socket,
            None => {
                debug!("bind: no socket found for fd: {}", &sockfd);
                return unsafe { libc::bind(sockfd, addr, addrlen) };
            }
        }
    };

    let raw_addr = unsafe { OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize) };
    let parsed_addr = match raw_addr.into_addr() {
        Some(addr) => addr,
        None => {
            error!("bind: failed to parse addr");
            return libc::EINVAL;
        }
    };

    debug!("bind:port: {}", parsed_addr.port());
    if is_ignored_port(parsed_addr.port()) {
        debug!("bind: ignoring port: {}", parsed_addr.port());
        return unsafe { libc::bind(sockfd, addr, addrlen) };
    }

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound {
        address: parsed_addr,
    });

    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(sockfd, socket);
    0
}
/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn listen(sockfd: RawFd, _backlog: c_int) -> c_int {
    debug!("listen called");
    let mut socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.remove(&sockfd) {
            Some(socket) => socket,
            None => {
                debug!("listen: no socket found for fd: {}", &sockfd);
                return unsafe { libc::listen(sockfd, _backlog) };
            }
        }
    };
    match &socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();
            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(*bound);
            let mut os_addr = match socket.domain {
                libc::AF_INET => {
                    OsSocketAddr::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                }
                libc::AF_INET6 => {
                    OsSocketAddr::from(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
                }
                _ => {
                    // shouldn't happen
                    debug!("unsupported domain");
                    return libc::EINVAL;
                }
            };

            let ret = unsafe { libc::bind(sockfd, os_addr.as_ptr(), os_addr.len()) };
            if ret != 0 {
                error!(
                        "listen: failed to bind socket ret: {:?}, addr: {:?}, sockfd: {:?}, errno: {:?}",
                        ret, os_addr, sockfd, errno()
                    );
                return ret;
            }
            let mut addr_len = os_addr.len();
            // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
            // connect to.
            let ret = unsafe { libc::getsockname(sockfd, os_addr.as_mut_ptr(), &mut addr_len) };
            if ret != 0 {
                error!(
                    "listen: failed to get sockname ret: {:?}, addr: {:?}, sockfd: {:?}",
                    ret, os_addr, sockfd
                );
                return ret;
            }
            let result_addr = match os_addr.into_addr() {
                Some(addr) => addr,
                None => {
                    error!("listen: failed to parse addr");
                    return libc::EINVAL;
                }
            };
            let ret = unsafe { libc::listen(sockfd, _backlog) };
            if ret != 0 {
                error!(
                    "listen: failed to listen ret: {:?}, addr: {:?}, sockfd: {:?}",
                    ret, result_addr, sockfd
                );
                return ret;
            }
            let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
            match sender.blocking_send(HookMessage::Tcp(HookMessageTcp::Listen(Listen {
                fake_port: result_addr.port(),
                real_port,
                ipv6: result_addr.is_ipv6(),
                fd: sockfd,
            }))) {
                Ok(_) => {}
                Err(e) => {
                    error!("listen: failed to send listen message: {:?}", e);
                    return libc::EFAULT;
                }
            };
        }
        _ => {
            error!(
                "listen: socket is not bound or already listening, state: {:?}",
                socket.state
            );
            return libc::EINVAL;
        }
    }
    debug!("listen: success");
    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(sockfd, socket);
    0
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

    let user_socket = unsafe { socket2::Socket::from_raw_fd(sockfd) };

    if let SocketState::Initialized = user_socket_info.state {
        // TODO(alex) [high] 2022-07-15: Implementation plan
        // 1. Send connect message to `agent`;
        // 2. `agent` creates a socket to handle this `layer`->`agent` connection;
        // 3. `agent` creates another socket that calls `connect` to the `remote_address`;
        // 4. Calls to read on this socket in `agent` will contain data the we log, and then
        // write to `remote_address`;

        // We're creating an interceptor socket that talks with the user socket.
        let unbound_mirror_address = match user_socket.domain()? {
            Domain::IPV4 => Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            Domain::IPV6 => Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)),
            _ => Err(LayerError::UnsupportedDomain(user_socket_info.domain)),
        }?;

        let (channel_tx, channel_rx) = oneshot::channel();

        let mirror_listener: tokio::net::TcpListener =
            TcpListener::bind(unbound_mirror_address)?.try_into()?;

        let mirror_address = mirror_listener.local_addr()?;

        let connect = Connect {
            mirror_listener,
            user_fd: sockfd,
            remote_address,
            channel_tx,
        };

        let connect_hook = OutgoingTraffic::Connect(connect);
        blocking_send_hook_message(HookMessage::OutgoingTraffic(connect_hook))?;

        channel_rx.blocking_recv()??;

        let connected = Connected {
            remote_address,
            mirror_address,
        };

        // TODO(alex) [high] 2202-07-16:
        // // 1. We send a hook message to the agent;
        // 2. agent will create a `TcpListener` waiting for a connection on `intercept_address`;
        // 3. agent sends back to layer this address;
        // 4. layer connects to this intercepted address;
        //
        // Need a thread that will hold all these intercepted addresses in agent, so we can use
        // `set_namespace` in there.
        // let intercept_address = hook_channel_rx.recv()?;

        user_socket.connect(&mirror_address.into())?;
        Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);

        let mut sockets = SOCKETS.lock().unwrap();
        sockets.insert(sockfd, user_socket_info);

        Ok(())
    } else {
        user_socket.connect(&SockAddr::from(remote_address))
    }?;

    user_socket.into_raw_fd();

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
) -> c_int {
    debug!("getpeername called");
    let remote_address = {
        let sockets = SOCKETS.lock().unwrap();
        match sockets.get(&sockfd) {
            Some(socket) => match &socket.state {
                SocketState::Connected(connected) => connected.remote_address,
                _ => {
                    debug!(
                        "getpeername: socket is not connected, state: {:?}",
                        socket.state
                    );
                    set_errno(Errno(libc::ENOTCONN));
                    return -1;
                }
            },
            None => {
                debug!("getpeername: no socket found for fd: {}", &sockfd);
                return unsafe { libc::getpeername(sockfd, address, address_len) };
            }
        }
    };
    debug!("remote_address: {:?}", remote_address);
    fill_address(address, address_len, remote_address)
}
/// Resolve the fake local address to the real local address.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    debug!("getsockname called");
    let local_address = {
        let sockets = SOCKETS.lock().unwrap();
        match sockets.get(&sockfd) {
            Some(socket) => match &socket.state {
                SocketState::Connected(connected) => connected.mirror_address,
                SocketState::Bound(bound) => bound.address,
                SocketState::Listening(bound) => bound.address,
                _ => {
                    debug!(
                        "getsockname: socket is not bound or connected, state: {:?}",
                        socket.state
                    );
                    return unsafe { libc::getsockname(sockfd, address, address_len) };
                }
            },
            None => {
                debug!("getsockname: no socket found for fd: {}", &sockfd);
                return unsafe { libc::getsockname(sockfd, address, address_len) };
            }
        }
    };
    debug!("local_address: {:?}", local_address);
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
) -> RawFd {
    let (local_address, domain, protocol, type_) = {
        if let Some(socket) = SOCKETS.lock().unwrap().get(&sockfd) {
            if let SocketState::Listening(bound) = &socket.state {
                (bound.address, socket.domain, socket.protocol, socket.type_)
            } else {
                error!("original socket is not listening");
                return new_fd;
            }
        } else {
            debug!("origin socket not found");
            return new_fd;
        }
    };
    let socket_info = { CONNECTION_QUEUE.lock().unwrap().get(&sockfd) };
    let remote_address = match socket_info {
        Some(socket_info) => socket_info,
        None => {
            debug!("accept: socketinformation not found, probably not ours");
            return new_fd;
        }
    }
    .address;
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

    SOCKETS.lock().unwrap().insert(new_fd, Arc::new(new_socket));
    new_fd
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

            trace!("getaddrinfo -> c_addr_info {:#?}", c_addr_info);

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
