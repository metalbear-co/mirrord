use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::Arc,
};

use errno::{errno, set_errno, Errno};
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use tracing::{debug, error, warn};

use super::*;
use crate::{
    error::LayerError,
    message::HookMessage,
    tcp::{HookMessageTcp, Listen},
    HOOK_SENDER,
};

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
pub(super) fn socket(
    fd: RawFd,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> Result<RawFd, LayerError> {
    debug!(
        "socket -> fd {:#?} | domain {:#?} | type {:#?}",
        fd, domain, type_
    );

    // We don't handle non Tcpv4 sockets
    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        warn!(
            "socket-> Found non-Tcpv4 socket with values: domain {:#?} | type {:#?}",
            domain, type_
        );

        Err(LayerError::SocketNotTcpv4(fd))
    } else {
        let mut sockets = SOCKETS.lock().unwrap();
        sockets.insert(
            fd,
            Arc::new(Socket {
                domain,
                type_,
                protocol,
                state: SocketState::default(),
            }),
        );

        Ok(fd)
    }
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state and don't call bind (will be called later). In any other case, we call
/// regular bind.
pub(super) fn bind(socket: &mut Socket, raw_addr: OsSocketAddr) -> Result<(), LayerError> {
    debug!("bind -> socket {:#?} | raw_addr {:#?}", socket, raw_addr);

    if socket.state != SocketState::Initialized {
        Err(LayerError::SocketInvalidState(socket.state.clone()))
    } else {
        let parsed_addr = raw_addr
            .into_addr()
            .ok_or_else(|| {
                error!("bind -> Failed to parse raw_addr {:#?}!", raw_addr);
                LayerError::ParseSocketAddr(raw_addr)
            })
            .and_then(|parsed_addr| {
                let port = parsed_addr.port();
                debug!("bind -> port {:#?}", port);

                if is_ignored_port(port) {
                    Err(LayerError::IgnoredPort(parsed_addr))
                } else {
                    Ok(parsed_addr)
                }
            })?;

        socket.state = SocketState::Bound(Bound {
            address: parsed_addr,
        });

        Ok(())
    }
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn listen(socket: &mut Socket) -> Result<c_int, LayerError> {
    debug!("listen called");

    match &socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();
            socket.state = SocketState::Listening(*bound);

            let mut os_addr = match socket.domain {
                libc::AF_INET => {
                    OsSocketAddr::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                }
                libc::AF_INET6 => {
                    OsSocketAddr::from(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
                }
                invalid_domain => {
                    // shouldn't happen
                    warn!("unsupported domain");
                    return Err(LayerError::UnsupportedDomain(invalid_domain));
                }
            };

            // TODO(alex) [high] 2022-06-22: This is a though one, so many libc calls mixed in here.
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

#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn connect(sockfd: RawFd, address: *const sockaddr, len: socklen_t) -> c_int {
    debug!("connect -> sockfd {:#?} | len {:#?}", sockfd, len);

    let socket = {
        let mut sockets = SOCKETS.lock().unwrap();

        match sockets.remove(&sockfd) {
            Some(socket) => socket,
            None => {
                warn!("connect: no socket found for fd: {}", &sockfd);
                return unsafe { libc::connect(sockfd, address, len) };
            }
        }
    };

    // We don't handle this socket, so restore state if there was any. (delay execute bind)
    if let SocketState::Bound(bound) = &socket.state {
        let os_addr = OsSocketAddr::from(bound.address);

        let ret = unsafe { libc::bind(sockfd, os_addr.as_ptr(), os_addr.len()) };

        if ret != 0 {
            error!(
                "connect: failed to bind socket ret: {:?}, addr: {:?}, sockfd: {:?}",
                ret, os_addr, sockfd
            );

            return ret;
        }
    };
    unsafe { libc::connect(sockfd, address, len) }
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
                SocketState::Connected(connected) => connected.local_address,
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
    let new_socket = Socket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            local_address,
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
